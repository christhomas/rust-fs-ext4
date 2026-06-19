//! Format a fresh filesystem with the driver's own `mkfs::format_filesystem`
//! and leave it in /tmp for a real Linux e2fsck pass:
//!
//!   scripts/vm-e2fsck.sh /tmp/fs_ext4_mkfs_*.img
//!
//! `mkfs_roundtrip` and `mkfs_bin_smoke` already format + re-mount through the
//! driver's OWN reader, but that reader can't see a wrong checksum (the exact
//! blind spot that hid the Pi corruption). mkfs writes a large checksum surface
//! from scratch — superblock csum, the group-descriptor table, the group's
//! block/inode bitmap csums, the root inode csum, and the root dir-block tail —
//! none of which had ever faced an external checker (mkfs_bin_smoke's header
//! says so: "when one is wired up"). This is that checker.
//!
//! mkfs is single-block-group only, so the meaningful axis is block size; the
//! 1 KiB case uses the distinct first_data_block=1 layout and a minimal
//! journal. All produced images carry metadata_csum + metadata_csum_seed.

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::fs::Filesystem;
use fs_ext4::mkfs;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const UUID: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE,
];

/// Pre-size a tmp file, format it via the driver's mkfs, and return its path.
fn format_to_tmp(tag: &str, size: u64, block_size: u32) -> Option<String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let path = format!("/tmp/fs_ext4_mkfs_{tag}_{}_{n}.img", std::process::id());
    {
        let f = std::fs::File::create(&path).ok()?;
        f.set_len(size).ok()?;
    }
    {
        let dev = FileDevice::open_rw(&path).expect("open_rw");
        mkfs::format_filesystem(&dev, Some("MKFSORACLE"), Some(UUID), size, block_size)
            .expect("format_filesystem");
        dev.flush().expect("flush");
    } // drop closes the file → bytes are on disk
    Some(path)
}

/// Mount the freshly-formatted image through the driver and sanity-check the
/// root, then leave it for the external e2fsck (or clean up).
fn check_and_done(path: &str, tag: &str, block_size: u32) {
    {
        let dev = FileDevice::open(path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount fresh fs");
        assert_eq!(fs.sb.block_size(), block_size, "[{tag}] block size");
        assert!(fs.csum.enabled, "[{tag}] metadata_csum must be on");
        assert!(fs.sb.is_clean(), "[{tag}] fresh fs must be clean");
        let (root, _) = fs.read_inode_verified(2).expect("root inode verifies");
        assert!(root.is_dir(), "[{tag}] root must be a directory");
        assert_eq!(root.links_count, 2, "[{tag}] root links = 2");

        // Structural audit: the freshly-formatted block/inode bitmaps and the
        // stored free counters must already agree. This catches the
        // first_data_block=1 bitmap/count drift that e2fsck flags as "Free
        // blocks count wrong" on 1 KiB-block images, without needing the VM.
        let report = fs_ext4::fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit");
        assert!(
            report.is_clean(),
            "[{tag}] fresh fs has structural anomalies: {:?}",
            report.anomalies
        );
    }
    if std::env::var_os("RFE_KEEP_IMAGES").is_some() {
        eprintln!("[{tag}] image: {path}");
    } else {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn mkfs_4k_blocks_32m() {
    let Some(p) = format_to_tmp("4k", 32 * 1024 * 1024, 4096) else {
        return;
    };
    check_and_done(&p, "4k", 4096);
}

#[test]
fn mkfs_2k_blocks_16m() {
    let Some(p) = format_to_tmp("2k", 16 * 1024 * 1024, 2048) else {
        return;
    };
    check_and_done(&p, "2k", 2048);
}

#[test]
fn mkfs_1k_blocks_8m() {
    // 1 KiB blocks → first_data_block=1, minimal (1024-block) journal: a
    // distinct on-disk layout from the 4 KiB default.
    let Some(p) = format_to_tmp("1k", 8 * 1024 * 1024, 1024) else {
        return;
    };
    check_and_done(&p, "1k", 1024);
}
