//! Detection-side coverage for `WrongDotDot` on non-root directories.
//!
//! The audit's pre-existing WrongDotDot check only flagged the root
//! inode — every other directory was skipped with a "needs more
//! tracking" comment. This test fabricates the simplest non-root
//! corruption (subdir's ".." dirent claims a parent inode that
//! doesn't exist) and asserts the audit now surfaces it.
//!
//! Repair is intentionally out of scope here — this PR is detection
//! only, paired with a follow-up that wires the rewrite into the
//! repair pass once that infrastructure lands.
//!
//! Helpers are inlined rather than shared because the broader
//! corruption-fabrication test machinery (find_dirent_slot, raw
//! poke, dir-csum recompute) doesn't live on main yet — it ships
//! alongside the repair-pass work in a separate branch. Keeping this
//! file self-contained avoids cross-dependency churn at review time.

use fs_ext4::block_io::FileDevice;
use fs_ext4::extent;
use fs_ext4::features;
use fs_ext4::fs::Filesystem;
use fs_ext4::fsck::Anomaly;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, slot: &str) -> Option<String> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!(
        "/tmp/fs_ext4_wrong_dotdot_{}_{slot}_{n}.img",
        std::process::id()
    );
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// Find the on-disk byte offset of a dirent matching `name` inside
/// `dir_ino`. Returns (physical_block, offset_in_block, current_inode).
/// Used to fabricate corruption by patching the inode field directly.
fn find_dirent_slot(fs: &Filesystem, dir_ino: u32, name: &[u8]) -> (u64, usize, u32) {
    let (inode, _) = fs.read_inode_verified(dir_ino).expect("read dir inode");
    let has_ft = fs.sb.feature_incompat & features::Incompat::FILETYPE.bits() != 0;
    let bs = fs.sb.block_size();
    let total = inode.size.div_ceil(bs as u64);
    let mut buf = vec![0u8; bs as usize];
    for logical in 0..total {
        let Some(phys) =
            extent::map_logical(&inode.block, fs.dev.as_ref(), bs, logical).expect("map")
        else {
            continue;
        };
        fs.dev.read_at(phys * bs as u64, &mut buf).expect("read");
        let mut off = 0usize;
        while off + 8 <= buf.len() {
            let cur_inode = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
            let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap()) as usize;
            if rec_len < 8 || !rec_len.is_multiple_of(4) || off + rec_len > buf.len() {
                break;
            }
            if cur_inode != 0 {
                let cur_name_lo = buf[off + 6];
                let cur_type_or_hi = buf[off + 7];
                let cur_name_len = if has_ft {
                    cur_name_lo as usize
                } else {
                    ((cur_type_or_hi as usize) << 8) | cur_name_lo as usize
                };
                if 8 + cur_name_len <= rec_len && &buf[off + 8..off + 8 + cur_name_len] == name {
                    return (phys, off, cur_inode);
                }
            }
            off += rec_len;
        }
    }
    panic!("dirent {:?} not found in dir ino {}", name, dir_ino);
}

/// Patch a four-byte inode field at (phys_block, off) by reopening
/// the image at the OS level. Bypasses the filesystem so the next
/// mount sees the patched bytes.
fn poke_inode_field_raw(
    image_path: &str,
    block_size: u32,
    phys_block: u64,
    off: usize,
    new_inode: u32,
) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_path)
        .expect("open raw");
    f.seek(SeekFrom::Start(phys_block * block_size as u64 + off as u64))
        .expect("seek");
    f.write_all(&new_inode.to_le_bytes()).expect("write");
}

/// Rewrite the dir-csum tail for `phys_block` so the patched block
/// re-validates. Mirrors `Checksummer::verify_dir_entry_tail`: crc32c
/// over bytes 0 .. block_size-12, seeded with crate seed → ino → gen.
fn fix_dir_csum_after_poke(image_path: &str, fs: &Filesystem, dir_ino: u32, phys_block: u64) {
    use std::io::{Seek, SeekFrom, Write};
    if !fs.csum.enabled {
        return;
    }
    let bs = fs.sb.block_size() as usize;
    let block = fs.read_block(phys_block).expect("read block");
    if !fs_ext4::dir::has_csum_tail(&block) {
        return;
    }
    let (inode, _) = fs.read_inode_verified(dir_ino).expect("read inode");
    let mut c = fs_ext4::checksum::linux_crc32c(fs.csum.seed, &dir_ino.to_le_bytes());
    c = fs_ext4::checksum::linux_crc32c(c, &inode.generation.to_le_bytes());
    c = fs_ext4::checksum::linux_crc32c(c, &block[..bs - 12]);
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_path)
        .expect("open raw");
    f.seek(SeekFrom::Start(phys_block * bs as u64 + (bs as u64 - 4)))
        .expect("seek");
    f.write_all(&c.to_le_bytes()).expect("write csum");
}

/// End-to-end: create a subdir, patch its ".." to claim a fake
/// parent, run the audit, and assert WrongDotDot is reported with
/// the right shape (dir_ino = the subdir, claims = the fake inode,
/// actual_parent = root).
#[test]
fn audit_flags_wrong_dotdot_on_non_root_dir() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "wrongdotdot") else {
        eprintln!("skip: ext4-basic.img not present");
        return;
    };

    let subdir_ino;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        subdir_ino = fs
            .apply_mkdir("/wrongdotdot_test", 0o755)
            .expect("mkdir test dir");
        // fs drop flushes journaled writes.
    }

    // Find ".." dirent slot in the new subdir.
    let dotdot_slot;
    let bs;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 2");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 2");
        dotdot_slot = find_dirent_slot(&fs, subdir_ino, b"..");
        bs = fs.sb.block_size();
        // Sanity: a fresh subdir's ".." points at root.
        assert_eq!(
            dotdot_slot.2,
            fs_ext4::path::EXT4_ROOT_INODE,
            "fresh subdir's .. must point at root before patch"
        );
    }

    // Inject the corruption: claim a fake parent inode (one that
    // certainly doesn't exist in this image). The detection compares
    // the claim against the walker's actual_parent map, so any value
    // other than 2 triggers.
    let fake_parent: u32 = 99;
    poke_inode_field_raw(&path, bs, dotdot_slot.0, dotdot_slot.1, fake_parent);

    // Recompute the dir-block CRC tail so the audit's mount path
    // doesn't reject the block as bad-csum, which would mask the
    // wrong-dotdot signal we want to test.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 3");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 3");
        fix_dir_csum_after_poke(&path, &fs, subdir_ino, dotdot_slot.0);
    }

    // Audit the corrupted image. Read-only is sufficient — audit is a
    // diagnostic pass and any accidental write would surface as failure.
    {
        let dev = FileDevice::open(&path).expect("open ro audit");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount audit");
        let report = fs.audit(u32::MAX, u32::MAX).expect("audit");
        let detected = report.anomalies.iter().any(|a| {
            matches!(a,
                Anomaly::WrongDotDot { dir_ino, claims, actual_parent }
                    if *dir_ino == subdir_ino
                        && *claims == fake_parent
                        && *actual_parent == fs_ext4::path::EXT4_ROOT_INODE
            )
        });
        assert!(
            detected,
            "expected WrongDotDot for subdir {subdir_ino} (got {:#?})",
            report.anomalies
        );
    }

    let _ = std::fs::remove_file(&path);
}
