//! Format ext3 (and ext2) volumes with the driver's own mkfs and hand each to
//! `e2fsck -fn`, which must exit 0. Where e2fsprogs is not installed the
//! in-process checks still run and the external one is skipped with a note.
//!
//! `mkfs_e2fsck_oracle` covers the default Ext4 flavor; this covers the legacy
//! flavors, which take materially different code paths:
//!
//!   * Ext3 — HAS_JOURNAL (a real jbd2 log on the hidden journal inode #8,
//!     mapped with legacy indirect blocks, not extents), 128-byte inodes,
//!     32-byte group descriptors, no metadata_csum.
//!   * Ext2 — same legacy layout but no journal.
//!
//! The ext3 cases used to be ignored for a journal e2fsck called invalid:
//! mkfs wrote inode 8 with `i_mode = 0`, and e2fsck (like the kernel) takes a
//! journal inode that is not a regular file for no journal at all (#89). The
//! ext3 cases also write through the crate, so the journal the writer commits
//! to faces the checker too.

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::features::FsFlavor;
use fs_ext4::fs::Filesystem;
use fs_ext4::mkfs;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const UUID: [u8; 16] = [
    0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18, 0x29, 0x3A, 0x4B, 0x5C, 0x6D, 0x7E, 0x8F, 0x90,
];

fn format(tag: &str, size: u64, block_size: u32, flavor: FsFlavor) -> Option<String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let path =
        fs_ext4_test_support::temp_path!("fs_ext4_mkfsflav_{tag}_{}_{n}.img", std::process::id());
    {
        let f = std::fs::File::create(&path).ok()?;
        f.set_len(size).ok()?;
    }
    {
        let dev = FileDevice::open_rw(&path).expect("open_rw");
        mkfs::format_filesystem_with_flavor(
            &dev,
            Some("FLAVOR"),
            Some(UUID),
            size,
            block_size,
            flavor,
        )
        .expect("format_filesystem_with_flavor");
        dev.flush().expect("flush");
    }
    Some(path)
}

fn check_and_done(path: &str, tag: &str, block_size: u32, expect_journal: bool) {
    {
        let dev = FileDevice::open(path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount fresh fs");
        assert_eq!(fs.sb.block_size(), block_size, "[{tag}] block size");
        assert!(
            !fs.csum.enabled,
            "[{tag}] legacy flavors must not advertise metadata_csum"
        );
        assert!(fs.sb.is_clean(), "[{tag}] fresh fs must be clean");
        let (root, _) = fs.read_inode_verified(2).expect("root inode verifies");
        assert!(root.is_dir(), "[{tag}] root must be a directory");
        assert_eq!(root.links_count, 2, "[{tag}] root links = 2");

        // For ext3, a jbd2 journal superblock must be present and consistent.
        let jsb = fs_ext4::jbd2::read_superblock(&fs).expect("jsb read");
        if expect_journal {
            let j = jsb.expect("[ext3] expected a journal superblock");
            assert!(j.is_clean(), "[{tag}] fresh journal must be clean");
        } else {
            assert!(jsb.is_none(), "[{tag}] ext2 must have no journal");
        }

        // Structural audit: fresh bitmaps + free counters must already agree.
        let report = fs_ext4::fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit");
        assert!(
            report.is_clean(),
            "[{tag}] fresh fs has structural anomalies: {:?}",
            report.anomalies
        );
    }
    fs_ext4_test_support::assert_e2fsck_clean(path, tag);
    if expect_journal {
        write_through_the_journal(path, tag);
        fs_ext4_test_support::assert_e2fsck_clean(path, &format!("{tag} after writes"));
    }
    if std::env::var_os("RFE_KEEP_IMAGES").is_some() {
        eprintln!("[{tag}] image: {path}");
    } else {
        let _ = fs::remove_file(path);
    }
}

/// A directory, a file with content and a rename, each committed through the
/// ext3 journal.
fn write_through_the_journal(path: &str, tag: &str) {
    let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(path).expect("rw")))
        .unwrap_or_else(|e| panic!("[{tag}] mount rw: {e:?}"));
    fs.apply_mkdir("/d", 0o755).expect("mkdir");
    fs.apply_create("/d/f", 0o644).expect("create");
    fs.apply_replace_file_content("/d/f", &vec![0xA5; 20_000])
        .expect("write");
    fs.apply_rename("/d/f", "/g", false).expect("rename");
    // Enough names to grow /d past its first block, and a symlink too long
    // to live in i_block.
    for i in 0..300 {
        fs.apply_create(&format!("/d/a_longer_file_name_{i:04}"), 0o644)
            .unwrap_or_else(|e| panic!("[{tag}] create {i}: {e:?}"));
    }
    fs.apply_symlink(&"t".repeat(200), "/s").expect("symlink");
    let jsb = fs_ext4::jbd2::read_superblock(&fs)
        .expect("jsb read")
        .expect("a journal");
    assert!(
        jsb.sequence > 1,
        "[{tag}] the writes went through the journal"
    );
}

#[test]
fn mkfs_ext3_4k_blocks() {
    let Some(p) = format("ext3_4k", 32 * 1024 * 1024, 4096, FsFlavor::Ext3) else {
        return;
    };
    check_and_done(&p, "ext3_4k", 4096, true);
}

/// 1 KiB blocks additionally exercise the first_data_block=1 layout (the
/// free-count arithmetic accounts for the journal's indirect-tree blocks there
/// too).
#[test]
fn mkfs_ext3_1k_blocks() {
    let Some(p) = format("ext3_1k", 8 * 1024 * 1024, 1024, FsFlavor::Ext3) else {
        return;
    };
    check_and_done(&p, "ext3_1k", 1024, true);
}

#[test]
fn mkfs_ext2_4k_blocks() {
    let Some(p) = format("ext2_4k", 32 * 1024 * 1024, 4096, FsFlavor::Ext2) else {
        return;
    };
    check_and_done(&p, "ext2_4k", 4096, false);
}
