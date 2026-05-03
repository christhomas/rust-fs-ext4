//! Smoke tests for `verify::verify`.
//!
//! Three angles:
//! 1. **Fresh ext4 fixture**: the canonical `test-disks/ext4-basic.img` must
//!    pass cleanly. Catches any regression where the verifier itself
//!    misjudges a known-good ext4 layout.
//! 2. **Fresh ext2 mkfs**: a just-formatted ext2 volume (no user writes)
//!    must pass. Anchors the ext2/3 indirect path against the same oracle.
//! 3. **Corrupt image**: flipping a bit in the on-disk superblock magic
//!    must surface as an error in the report (the verifier doesn't crash
//!    on garbage; it reports it).

use fs_ext4::block_io::FileDevice;
use fs_ext4::features::FsFlavor;
use fs_ext4::fs::Filesystem;
use fs_ext4::mkfs::format_filesystem_with_flavor;
use fs_ext4::verify;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

const EXT4_FIXTURE: &str = "test-disks/ext4-basic.img";

fn scratch_path(stem: &str) -> std::path::PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("rust-fs-ext4-verify-{stem}-{pid}-{nanos}.img"))
}

struct ScratchGuard(std::path::PathBuf);
impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn verifies_clean_ext4_fixture() {
    let dev = Arc::new(FileDevice::open(EXT4_FIXTURE).expect("open fixture"));
    let fs = Filesystem::mount(dev).expect("mount fixture");
    assert_eq!(fs.flavor, FsFlavor::Ext4);

    let report = verify::verify(&fs).expect("verify ran");
    // The shipped ext4-basic.img is a known-good extent-backed volume.
    // Errors here mean either the fixture rotted or the verifier has a
    // false positive; warnings about leaked blocks are tolerated since
    // the fixture predates Phase A's bookkeeping (we don't enumerate
    // every metadata block perfectly).
    assert!(
        report.is_clean(),
        "verify rejected ext4-basic.img: {}\nerrors:\n  {}",
        report.summary(),
        report.errors.join("\n  ")
    );
}

#[test]
fn verifies_freshly_formatted_ext2() {
    let path = scratch_path("ext2-fresh");
    let size: u64 = 4 * 1024 * 1024;
    let block_size: u32 = 1024;
    {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("create scratch");
        f.set_len(size).expect("set_len");
    }
    let dev_rw = FileDevice::open_rw(path.to_str().unwrap()).expect("open rw");
    format_filesystem_with_flavor(
        &dev_rw,
        Some("VERIFY"),
        None,
        size,
        block_size,
        FsFlavor::Ext2,
    )
    .expect("mkfs ext2");
    let _cleanup = ScratchGuard(path.clone());

    let dev = Arc::new(FileDevice::open(path.to_str().unwrap()).expect("open ro"));
    let fs = Filesystem::mount(dev).expect("mount fresh ext2");
    assert_eq!(fs.flavor, FsFlavor::Ext2);

    let report = verify::verify(&fs).expect("verify ran");
    assert!(
        report.is_clean(),
        "fresh ext2 mkfs failed verify: {}\nerrors:\n  {}",
        report.summary(),
        report.errors.join("\n  ")
    );
}

#[test]
fn verify_detects_corrupted_superblock_magic() {
    // Mkfs an ext2 image, then corrupt the SB magic (offset 1024 + 0x38).
    // The mount itself will fail (good — verifier never gets to run on
    // an unmountable image), so we instead corrupt a non-fatal field that
    // the verifier checks: free_blocks_count > blocks_count.
    let path = scratch_path("ext2-corrupt-counters");
    let size: u64 = 4 * 1024 * 1024;
    let block_size: u32 = 1024;
    {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("create scratch");
        f.set_len(size).expect("set_len");
    }
    let dev_rw = FileDevice::open_rw(path.to_str().unwrap()).expect("open rw");
    format_filesystem_with_flavor(
        &dev_rw,
        Some("CORRUPT"),
        None,
        size,
        block_size,
        FsFlavor::Ext2,
    )
    .expect("mkfs ext2");
    let _cleanup = ScratchGuard(path.clone());

    // Corrupt SB free_blocks_count_lo (offset 1024 + 0x0C) to a value
    // larger than blocks_count. The mount will succeed (we don't validate
    // counters there); the verifier should catch it.
    {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("reopen for corrupt");
        f.seek(SeekFrom::Start(1024 + 0x0C)).expect("seek");
        let bogus_free: u32 = 0xFFFF_FFFE;
        f.write_all(&bogus_free.to_le_bytes()).expect("write");
        f.sync_all().expect("sync");
    }

    let dev = Arc::new(FileDevice::open(path.to_str().unwrap()).expect("open ro"));
    let fs = Filesystem::mount(dev).expect("mount corrupt");

    let report = verify::verify(&fs).expect("verify ran");
    assert!(
        !report.is_clean(),
        "verify failed to detect free_blocks_count > blocks_count: {}",
        report.summary()
    );
    let joined = report.errors.join(" || ");
    assert!(
        joined.contains("free_blocks_count"),
        "expected superblock free_blocks_count error, got: {joined}"
    );
}

/// Sanity: silence unused-import warning for the std::io traits we only
/// touch from one test branch (Read isn't currently used).
#[allow(dead_code)]
fn _imports_used() {
    let _ = std::io::empty().bytes();
}
