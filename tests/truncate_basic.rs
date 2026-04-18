//! End-to-end truncate test.
//!
//! Exercises the full Phase 4 shrink path: plan_truncate_shrink →
//! extent-mutation apply → bitmap bit clear → inode patch. Uses a writable
//! copy of ext4-basic.img so no other tests see the mutation.

use fs_ext4::block_io::FileDevice;
use fs_ext4::path as path_mod;
use fs_ext4::Filesystem;

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    path_mod::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path)
        .unwrap_or_else(|e| panic!("resolve {path}: {e}"))
}
use std::fs;
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str) -> Option<String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!("/tmp/fs_ext4_trunc_{}_{n}_{}.img", std::process::id(), name);
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

#[test]
fn truncate_read_only_device_rejected() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, "/test.txt");
    let err = fs.apply_truncate_shrink(ino, 4).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("read-only"), "unexpected error: {msg}");
    fs::remove_file(path).ok();
}

#[test]
fn truncate_zero_frees_blocks_and_clears_size() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    let ino = resolve(&fs, "/test.txt");
    let (before_inode, _) = fs.read_inode_verified(ino).expect("read inode");
    assert!(before_inode.size > 0, "fixture must have non-zero size");

    fs.apply_truncate_shrink(ino, 0).expect("truncate to 0");

    // Re-read the inode to verify on-disk state.
    let (after_inode, _) = fs.read_inode_verified(ino).expect("re-read inode");
    assert_eq!(after_inode.size, 0, "size not reset to 0");
    assert_eq!(after_inode.blocks, 0, "blocks counter not reset");

    fs::remove_file(path).ok();
}

#[test]
fn truncate_survives_remount() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };

    // First mount: truncate /test.txt to 4 bytes (file was "hello from ext4.\n").
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 4).expect("truncate");
    }

    // Second mount: size must still be 4.
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read inode");
    assert_eq!(inode.size, 4, "truncate did not persist across remount");

    fs::remove_file(path).ok();
}

#[test]
fn truncate_grow_direction_rejected() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read inode");
    // apply_truncate_shrink refuses growth.
    let err = fs
        .apply_truncate_shrink(ino, inode.size + 4096)
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("new_size > old_size"),
        "unexpected error: {msg}"
    );
    fs::remove_file(path).ok();
}
