//! Phase 5.2.2 – 5.2.5: every single-inode-block mutating op now flows
//! through the journal writer in production. This file pins that the
//! journal sequence advances whenever those ops fire — if a future change
//! accidentally bypasses `commit_inode_write` and goes back to a direct
//! `write_inode_raw`, the relevant assertion here trips.

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, tag: &str) -> Option<String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!("/tmp/fs_ext4_jw_iops_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// Snapshot the JBD2 sequence number from the on-disk journal SB.
/// Returns `None` when the image has no journal — caller should skip the
/// assertion in that case.
fn jsb_sequence(path: &str) -> Option<u32> {
    let dev = FileDevice::open(path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    fs_ext4::jbd2::read_superblock(&fs)
        .expect("read jsb")
        .map(|j| j.sequence)
}

fn run_with_writable<F>(path: &str, f: F)
where
    F: FnOnce(&Filesystem),
{
    let dev = FileDevice::open_rw(path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    f(&fs);
}

#[test]
fn apply_chown_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "chown") else {
        return;
    };
    let Some(seq_before) = jsb_sequence(&path) else {
        fs::remove_file(path).ok();
        return;
    };
    run_with_writable(&path, |fs| {
        fs.apply_chown("/test.txt", 1000, 1000).expect("chown");
    });
    let seq_after = jsb_sequence(&path).unwrap();
    assert!(
        seq_after > seq_before,
        "chown didn't advance jsb.sequence ({seq_before} -> {seq_after})"
    );
    fs::remove_file(path).ok();
}

#[test]
fn apply_utimens_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "utimens") else {
        return;
    };
    let Some(seq_before) = jsb_sequence(&path) else {
        fs::remove_file(path).ok();
        return;
    };
    run_with_writable(&path, |fs| {
        fs.apply_utimens("/test.txt", 1_700_000_000, 0, 1_700_000_000, 0)
            .expect("utimens");
    });
    let seq_after = jsb_sequence(&path).unwrap();
    assert!(
        seq_after > seq_before,
        "utimens didn't advance jsb.sequence ({seq_before} -> {seq_after})"
    );
    fs::remove_file(path).ok();
}

#[test]
fn apply_setxattr_inline_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "setxattr_inline") else {
        return;
    };
    let Some(seq_before) = jsb_sequence(&path) else {
        fs::remove_file(path).ok();
        return;
    };
    run_with_writable(&path, |fs| {
        // Tiny value fits in the in-inode region — exercises the inline path,
        // not the external-block fallback.
        fs.apply_setxattr("/test.txt", "user.k", b"v")
            .expect("setxattr");
    });
    let seq_after = jsb_sequence(&path).unwrap();
    assert!(
        seq_after > seq_before,
        "setxattr (inline) didn't advance jsb.sequence ({seq_before} -> {seq_after})"
    );
    fs::remove_file(path).ok();
}

#[test]
fn apply_truncate_grow_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "tgrow") else {
        return;
    };
    let Some(seq_before) = jsb_sequence(&path) else {
        fs::remove_file(path).ok();
        return;
    };
    run_with_writable(&path, |fs| {
        // Resolve the inode for /test.txt and grow it (sparse).
        let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
        let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt")
            .expect("lookup");
        fs.apply_truncate_grow(ino, 1024 * 1024)
            .expect("truncate_grow");
    });
    let seq_after = jsb_sequence(&path).unwrap();
    assert!(
        seq_after > seq_before,
        "truncate_grow didn't advance jsb.sequence ({seq_before} -> {seq_after})"
    );
    fs::remove_file(path).ok();
}

#[test]
fn journal_stays_clean_after_each_op() {
    // No matter which single-inode op runs, the journal should self-checkpoint
    // back to clean (jsb.start == 0) by the time we re-mount. Critical: a
    // dirty journal forces replay on every subsequent mount, which would
    // mask correctness bugs in our writer.
    let Some(path) = copy_to_tmp("ext4-basic.img", "stays_clean") else {
        return;
    };
    if jsb_sequence(&path).is_none() {
        fs::remove_file(path).ok();
        return;
    }
    run_with_writable(&path, |fs| {
        fs.apply_chmod("/test.txt", 0o600).expect("chmod");
        fs.apply_chown("/test.txt", 1, 1).expect("chown");
        fs.apply_utimens("/test.txt", 1_700_000_001, 0, 1_700_000_001, 0)
            .expect("utimens");
        fs.apply_setxattr("/test.txt", "user.k", b"v")
            .expect("setxattr");
    });
    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let jsb = fs_ext4::jbd2::read_superblock(&fs)
        .expect("read jsb")
        .expect("present");
    assert!(
        jsb.is_clean(),
        "journal not clean after batch of single-inode ops (start={})",
        jsb.start
    );
    fs::remove_file(path).ok();
}
