//! Phase 5.2.5 + 5.2.10 + 5.2.15 sequence-advance regression coverage.
//!
//! These ops were converted from un-journaled disk-write paths to the
//! BlockBuffer + JournalWriter pattern in commit a9fe115. Functional
//! coverage already exists (capi_rename_semantics, capi_callback_rw,
//! xattr_external_block, etc.) — but those don't assert that the
//! journal sequence advanced, so a regression to direct-write would go
//! silent until a crash exposed it.
//!
//! Each test here:
//!   1. Snapshots `jsb.sequence` before the op.
//!   2. Runs the op on a writable mount.
//!   3. Re-mounts read-only and asserts `jsb.sequence` advanced AND the
//!      journal is back to clean.

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, tag: &str) -> Option<String> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!("/tmp/fs_ext4_jw_rwx_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn jsb_seq(path: &str) -> Option<u32> {
    let dev = FileDevice::open(path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    fs_ext4::jbd2::read_superblock(&fs)
        .expect("jsb")
        .map(|j| j.sequence)
}

fn assert_clean(path: &str, tag: &str) {
    let dev = FileDevice::open(path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    if let Some(jsb) = fs_ext4::jbd2::read_superblock(&fs).expect("jsb") {
        assert!(
            jsb.is_clean(),
            "[{tag}] journal not clean (start={})",
            jsb.start
        );
    }
}

#[test]
fn rename_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "rename") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_rename("/test.txt", "/renamed.txt")
            .expect("rename");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(
            a > b,
            "rename did not advance jsb.sequence ({b} -> {a}); \
             multi-block path bypassed the writer"
        );
    }
    // Verify the rename actually took effect.
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/renamed.txt")
        .expect("renamed file should be reachable");
    assert!(
        fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt").is_err(),
        "src should be gone"
    );
    assert_clean(&path, "rename");
    fs::remove_file(path).ok();
}

#[test]
fn replace_file_content_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "rfc") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let payload = b"replaced content via journaled multi-block tx";
        fs.apply_replace_file_content("/test.txt", payload)
            .expect("replace");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(
            a > b,
            "replace_file_content did not advance jsb.sequence ({b} -> {a})"
        );
    }
    // Verify the new content reads back.
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt")
        .expect("test.txt still there");
    let (inode, _) = fs.read_inode_verified(ino).expect("read inode");
    let mut buf = vec![0u8; inode.size as usize];
    fs_ext4::file_io::read(&fs, &inode, 0, inode.size, &mut buf).expect("read");
    assert_eq!(
        &buf[..],
        b"replaced content via journaled multi-block tx",
        "round-trip mismatch"
    );
    assert_clean(&path, "rfc");
    fs::remove_file(path).ok();
}

#[test]
fn external_block_setxattr_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "extxattr") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        // 512-byte value forces overflow into the external xattr block.
        let value = vec![0xABu8; 512];
        fs.apply_setxattr("/test.txt", "user.huge", &value)
            .expect("setxattr");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(
            a > b,
            "external-block setxattr did not advance jsb.sequence ({b} -> {a}); \
             Path B (alloc fresh block) regressed to direct disk writes"
        );
    }
    assert_clean(&path, "extxattr_alloc");

    // Now exercise Path A (rewrite existing block) — set a SECOND xattr
    // on the same inode that fits in the existing block. The xattr block
    // already exists, so this is a rewrite, not an alloc. Must still
    // advance the journal.
    let seq_before2 = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_setxattr("/test.txt", "user.tiny", b"v")
            .expect("setxattr 2");
    }
    let seq_after2 = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before2, seq_after2) {
        assert!(
            a > b,
            "external-block setxattr Path A (rewrite) did not advance jsb.sequence \
             ({b} -> {a})"
        );
    }
    assert_clean(&path, "extxattr_rewrite");
    fs::remove_file(path).ok();
}
