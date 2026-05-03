//! Phase 5.2.8 + 5.2.11 + 5.2.12 + 5.2.13: create / link / symlink /
//! mkdir all route through BlockBuffer + JournalWriter when the parent
//! dir has room (the in-place add path). Pinned: every op advances
//! `jsb.sequence` and the journal returns to clean.

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
    let dst = format!(
        "/tmp/fs_ext4_jw_dirops_{}_{tag}_{n}.img",
        std::process::id()
    );
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
fn create_advances_journal_and_persists() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "create") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_create("/created.txt", 0o644).expect("create");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(a > b, "create did not advance jsb.sequence ({b} -> {a})");
    }
    // Verify the file is reachable.
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/created.txt")
        .expect("created file should be reachable");
    assert_clean(&path, "create");
    fs::remove_file(path).ok();
}

#[test]
fn mkdir_advances_journal_and_persists() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "mkdir") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_mkdir("/newdir", 0o755).expect("mkdir");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(a > b, "mkdir did not advance jsb.sequence ({b} -> {a})");
    }
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/newdir")
        .expect("dir should be reachable");
    let (inode, _) = fs.read_inode_verified(ino).expect("read");
    assert!(inode.is_dir(), "/newdir should be a directory");
    assert_clean(&path, "mkdir");
    fs::remove_file(path).ok();
}

#[test]
fn link_advances_journal_and_increments_nlink() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "link") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    let nlink_before = {
        let dev = FileDevice::open(&path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
        let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt")
            .expect("lookup");
        fs.read_inode_verified(ino).expect("read").0.links_count
    };
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_link("/test.txt", "/test.link").expect("link");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(a > b, "link did not advance jsb.sequence ({b} -> {a})");
    }
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    let ino =
        fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt").expect("lookup");
    let (inode, _) = fs.read_inode_verified(ino).expect("read");
    assert_eq!(
        inode.links_count,
        nlink_before + 1,
        "link did not bump src nlink"
    );
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.link")
        .expect("link target should be reachable");
    assert_clean(&path, "link");
    fs::remove_file(path).ok();
}

#[test]
fn symlink_fast_path_advances_journal() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "sym_fast") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        // Short target → fast symlink (inline in i_block, no data block).
        fs.apply_symlink("short", "/sl.short").expect("symlink");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(a > b, "symlink did not advance jsb.sequence ({b} -> {a})");
    }
    assert_clean(&path, "sym_fast");
    fs::remove_file(path).ok();
}

#[test]
fn symlink_slow_path_advances_journal() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "sym_slow") else {
        return;
    };
    let seq_before = jsb_seq(&path);
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        // Long target → slow symlink (allocates a data block).
        let long: String = "x".repeat(120);
        fs.apply_symlink(&long, "/sl.long").expect("symlink");
    }
    let seq_after = jsb_seq(&path);
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(
            a > b,
            "slow symlink did not advance jsb.sequence ({b} -> {a})"
        );
    }
    assert_clean(&path, "sym_slow");
    fs::remove_file(path).ok();
}
