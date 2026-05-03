//! End-to-end ext2: format an empty image with `FsFlavor::Ext2`, mount it,
//! and verify the read path goes through the legacy direct/indirect block
//! mapping (not extents). Proves the indirect reader works against a real
//! on-disk layout — not just the hand-constructed buffers in the unit tests.
//!
//! Geometry: 4 MiB image, 1 KiB blocks, single block group (matches the
//! mkfs constraint). The root directory fits in one block, so the root
//! inode's `i_block[0]` is a direct pointer with the rest zero — exercises
//! the direct-pointer tier of the indirect reader.

use fs_ext4::block_io::FileDevice;
use fs_ext4::dir;
use fs_ext4::features::FsFlavor;
use fs_ext4::file_io;
use fs_ext4::fs::Filesystem;
use fs_ext4::inode::{Inode, InodeFlags};
use fs_ext4::mkfs::format_filesystem_with_flavor;
use fs_ext4::verify;
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

const ROOT_INODE: u32 = 2;

/// RAII scratch-file cleanup; ignores deletion errors (test may have
/// already failed and we don't want to mask the real assertion failure).
struct ScratchGuard(std::path::PathBuf);
impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Create a fresh ext2 image at `path` with the given size + block size.
fn mkfs_ext2_image(path: &std::path::Path, size_bytes: u64, block_size: u32) {
    // Pre-allocate the file to `size_bytes` then format it.
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(path)
        .expect("create scratch image");
    f.set_len(size_bytes).expect("set_len");
    drop(f);

    let dev = FileDevice::open_rw(path.to_str().unwrap()).expect("open scratch rw");
    format_filesystem_with_flavor(
        &dev,
        Some("EXT2TEST"),
        None,
        size_bytes,
        block_size,
        FsFlavor::Ext2,
    )
    .expect("mkfs ext2");
}

/// Build a unique scratch path under the OS temp dir. Avoids pulling in the
/// `tempfile` crate just for one integration test.
fn scratch_path(stem: &str) -> std::path::PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("rust-fs-ext4-{stem}-{pid}-{nanos}.img"))
}

#[test]
fn mkfs_ext2_then_mount_and_read_root() {
    let path = scratch_path("ext2-basic");
    let size: u64 = 4 * 1024 * 1024; // 4 MiB
    let block_size: u32 = 1024;
    mkfs_ext2_image(&path, size, block_size);
    // Best-effort cleanup at end of test.
    let _cleanup = ScratchGuard(path.clone());

    let dev = Arc::new(FileDevice::open(path.to_str().unwrap()).expect("open ro"));
    let fs = Filesystem::mount(dev).expect("mount ext2");

    // Volume identity assertions.
    assert_eq!(fs.flavor, FsFlavor::Ext2, "flavor must detect as ext2");
    assert_eq!(fs.sb.volume_name, "EXT2TEST");
    assert_eq!(fs.sb.block_size(), block_size);
    assert_eq!(fs.sb.inode_size, 128, "ext2 default inode size");
    assert_eq!(fs.sb.desc_size, 32, "ext2 must use 32-byte BGDs");

    // Root inode lives at ino 2; must be a directory using indirect blocks
    // (i.e. the EXTENTS_FL flag must NOT be set on an ext2 volume).
    let root_raw = fs.read_inode_raw(ROOT_INODE).expect("read root inode");
    let root = Inode::parse(&root_raw).expect("parse root inode");
    assert!(root.is_dir(), "root must be a directory");
    assert!(
        (root.flags & InodeFlags::EXTENTS.bits()) == 0,
        "ext2 root inode must NOT carry EXTENTS_FL (got flags 0x{:x})",
        root.flags,
    );

    // The direct pointer at i_block[0] should be non-zero — that's the
    // single block holding the root dir's `.` and `..` entries.
    let i_block_0 = u32::from_le_bytes(root.block[0..4].try_into().unwrap());
    assert!(
        i_block_0 != 0,
        "ext2 root inode i_block[0] must point at the dir data block"
    );

    // Read the directory contents. This MUST go through the indirect path
    // (file_io::read sees no EXTENTS_FL → falls into the indirect branch).
    let dir_data = file_io::read_all(&fs, &root).expect("read root dir via indirect");
    assert_eq!(
        dir_data.len() as u32,
        block_size,
        "single-block root directory"
    );

    // Parse the entries and assert `.` + `..` are present and both point at
    // the root inode itself.
    let entries = dir::parse_block(&dir_data, true).expect("parse dir entries");
    let mut saw_dot = false;
    let mut saw_dotdot = false;
    for e in &entries {
        let name = std::str::from_utf8(&e.name).unwrap_or("<bad utf8>");
        match name {
            "." => {
                saw_dot = true;
                assert_eq!(e.inode, ROOT_INODE, ". entry must point at root");
            }
            ".." => {
                saw_dotdot = true;
                assert_eq!(e.inode, ROOT_INODE, ".. of root must point at root");
            }
            other => panic!("unexpected entry in fresh root dir: {other:?}"),
        }
    }
    assert!(saw_dot && saw_dotdot, "root must contain `.` and `..`");
}

/// Cover the four indirect-tree tiers in one test by stamping payloads that
/// straddle each tier boundary and round-tripping them through write → read.
///
/// Sizing (1 KiB blocks, ppb=256):
/// - "tiny" (1 block) → only `i_block[0]` populated.
/// - "direct" (12 blocks) → fills the direct pointers exactly.
/// - "single" (15 blocks) → 12 direct + 3 single-indirect.
/// - "double" (271 blocks) → 12 direct + 256 single + 3 double-indirect.
///
/// The "double" run forces the writer to allocate 1 single-indirect + 1
/// double-outer + 1 double-inner block — three indirect-tree blocks
/// co-allocated with 271 data blocks in a single contiguous run.
#[test]
fn mkfs_ext2_then_write_and_read_back_each_tier() {
    let path = scratch_path("ext2-write");
    let size: u64 = 8 * 1024 * 1024; // 8 MiB — fits 271-block file + metadata
    let block_size: u32 = 1024;
    mkfs_ext2_image(&path, size, block_size);
    let _cleanup = ScratchGuard(path.clone());

    // Mount RW so the writer can allocate + persist.
    let dev_rw = Arc::new(FileDevice::open_rw(path.to_str().unwrap()).expect("open rw"));
    let fs = Filesystem::mount(dev_rw).expect("mount ext2 rw");
    assert_eq!(fs.flavor, FsFlavor::Ext2);
    assert!(
        fs.dev.is_writable(),
        "test requires writable device for apply_create / apply_replace"
    );

    // Each case: (filename, byte_count). Byte counts are picked so that
    // size_in_blocks = ceil(bytes/block_size) hits the named tier exactly.
    let cases = [
        ("/tiny.bin", 200usize),                    // 1 block (direct only)
        ("/direct.bin", 12 * block_size as usize),  // 12 blocks (direct full)
        ("/single.bin", 15 * block_size as usize),  // 15 blocks (single-indirect)
        ("/double.bin", 271 * block_size as usize), // 271 blocks (double-indirect)
    ];

    for (name, byte_count) in cases {
        // Generate a deterministic payload — i-th byte = (i * 31 + name_hash) & 0xFF
        // so any byte misorder is detectable, and identical-length files don't
        // accidentally match if the writer mixed up files.
        let name_hash: u32 = name.bytes().map(|b| b as u32).sum();
        let payload: Vec<u8> = (0..byte_count)
            .map(|i| ((i as u32).wrapping_mul(31).wrapping_add(name_hash) & 0xFF) as u8)
            .collect();

        // 1. Create the file (allocates an inode; no data blocks yet).
        let new_ino = fs.apply_create(name, 0o644).expect("apply_create");
        let (inode_after_create, _) = fs.read_inode_verified(new_ino).expect("read new inode");
        assert!(
            (inode_after_create.flags & InodeFlags::EXTENTS.bits()) == 0,
            "{name}: freshly-created ext2 file inode must NOT carry EXTENTS_FL"
        );
        assert_eq!(inode_after_create.size, 0, "{name}: starts empty");

        // 2. Write the payload via the indirect dispatch path.
        let written = fs
            .apply_replace_file_content(name, &payload)
            .expect("apply_replace_file_content");
        assert_eq!(
            written as usize, byte_count,
            "{name}: apply_replace_file_content returned wrong length"
        );

        // 3. Re-read the inode + the file contents (round-trips through the
        //    indirect READ path, since EXTENTS_FL is still unset).
        let (inode_after_write, _) = fs.read_inode_verified(new_ino).expect("re-read inode");
        assert_eq!(
            inode_after_write.size, byte_count as u64,
            "{name}: inode size mismatch after write"
        );
        assert!(
            (inode_after_write.flags & InodeFlags::EXTENTS.bits()) == 0,
            "{name}: write must not have flipped EXTENTS_FL on (we use indirect)"
        );

        let read_back = file_io::read_all(&fs, &inode_after_write).expect("read_all via indirect");
        assert_eq!(
            read_back.len(),
            byte_count,
            "{name}: read-back length mismatch"
        );
        assert_eq!(
            read_back, payload,
            "{name}: byte-for-byte content mismatch through ext2 indirect roundtrip"
        );
    }

    // Final assertion: structural verifier must agree the volume is sane
    // after every tier of writes. Catches regressions where the writer
    // forgets to mark indirect-tree blocks as allocated, double-claims a
    // physical block, or strands data outside any inode's reach.
    let report = verify::verify(&fs).expect("verify walked the volume");
    assert!(
        report.is_clean(),
        "structural verifier rejected the post-write volume: {}\nerrors:\n  {}",
        report.summary(),
        report.errors.join("\n  ")
    );
}

/// `EXT4_FEATURE_COMPAT_HAS_JOURNAL` (bit 0x0004 in `feature_compat` at SB
/// offset 0x5C). Setting this bit on an ext2-formatted image is enough for
/// `FsFlavor::detect` to classify the volume as ext3, which is all these
/// tests need to exercise the Phase A ext3 mount-policy path.
const HAS_JOURNAL_BIT: u32 = 0x0004;

/// Patch an existing ext2 image into an "ext3-shaped" image by flipping
/// the HAS_JOURNAL bit in the on-disk superblock. We don't bother creating
/// a real journal inode — the Phase A code paths under test never read it
/// (RO mount skips the journal entirely; RW mount is refused before any
/// journal access is attempted).
fn flip_to_ext3_flavor(path: &std::path::Path) {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open image rw for patch");
    // Superblock lives at byte offset 1024; feature_compat at SB offset 0x5C.
    let off = 1024 + 0x5C;
    f.seek(SeekFrom::Start(off)).expect("seek sb feat_compat");
    let mut buf = [0u8; 4];
    use std::io::Read;
    f.read_exact(&mut buf).expect("read feat_compat");
    let mut feat = u32::from_le_bytes(buf);
    feat |= HAS_JOURNAL_BIT;
    f.seek(SeekFrom::Start(off)).expect("seek sb feat_compat");
    f.write_all(&feat.to_le_bytes()).expect("write feat_compat");
    f.sync_all().expect("sync");
}

#[test]
fn ext3_ro_mount_succeeds_via_flavor_detection() {
    let path = scratch_path("ext3-ro");
    let size: u64 = 4 * 1024 * 1024;
    let block_size: u32 = 1024;
    mkfs_ext2_image(&path, size, block_size);
    let _cleanup = ScratchGuard(path.clone());
    flip_to_ext3_flavor(&path);

    // Read-only mount: `replay_if_dirty` short-circuits on the writability
    // check before touching the journal inode (which would otherwise bail
    // because we never created one). FsFlavor::detect should classify the
    // volume as ext3 thanks to the HAS_JOURNAL bit we flipped on.
    let dev = Arc::new(FileDevice::open(path.to_str().unwrap()).expect("open ro"));
    let fs = Filesystem::mount(dev).expect("ext3 RO mount must succeed");
    assert_eq!(
        fs.flavor,
        FsFlavor::Ext3,
        "HAS_JOURNAL bit should yield Ext3 flavor"
    );
    assert!(!fs.dev.is_writable(), "test invariant: RO device");
    assert!(
        fs.journal.is_none(),
        "RO mount must not open the journal writer"
    );

    // Sanity: reads still work via the indirect path.
    let root_raw = fs.read_inode_raw(ROOT_INODE).expect("read root inode");
    let root = Inode::parse(&root_raw).expect("parse root inode");
    let dir_data = file_io::read_all(&fs, &root).expect("read root dir");
    let entries = dir::parse_block(&dir_data, true).expect("parse dir entries");
    assert!(entries.iter().any(|e| e.name == b"."));
    assert!(entries.iter().any(|e| e.name == b".."));
}

#[test]
fn ext3_rw_mount_refused_in_phase_a() {
    let path = scratch_path("ext3-rw-refused");
    let size: u64 = 4 * 1024 * 1024;
    let block_size: u32 = 1024;
    mkfs_ext2_image(&path, size, block_size);
    let _cleanup = ScratchGuard(path.clone());
    flip_to_ext3_flavor(&path);

    // Read-write mount: must refuse with the Phase A ext3 guard, NOT bail
    // deep in the journal walker. Either path returns an Err, but the
    // top-level guard yields a stable, user-facing message.
    let dev = Arc::new(FileDevice::open_rw(path.to_str().unwrap()).expect("open rw"));
    let err = match Filesystem::mount(dev) {
        Err(e) => e,
        Ok(_) => panic!("ext3 RW mount must be refused in Phase A"),
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("ext3 RW mount not supported"),
        "expected Phase A ext3 refusal message, got: {msg}"
    );
}
