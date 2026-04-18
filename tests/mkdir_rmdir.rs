//! End-to-end mkdir + rmdir test against a writable copy of ext4-basic.img.

use fs_ext4::block_io::FileDevice;
use fs_ext4::path as path_mod;
use fs_ext4::Filesystem;
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
    let dst = format!("/tmp/fs_ext4_mkdir_{}_{n}_{}.img", std::process::id(), name);
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> Option<u32> {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    path_mod::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).ok()
}

#[test]
fn mkdir_creates_dir_with_correct_nlink() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    let ino = fs.apply_mkdir("/fresh_dir", 0o755).expect("mkdir");
    assert!(ino > 0);

    let (new_inode, _) = fs.read_inode_verified(ino).expect("read new dir inode");
    assert!(new_inode.is_dir(), "mkdir target must be a directory");
    assert_eq!(
        new_inode.links_count, 2,
        "new dir nlink must be 2 (. + parent)"
    );
    assert_eq!(
        new_inode.size,
        fs.sb.block_size() as u64,
        "size == one data block"
    );

    // Parent root inode's nlink should have bumped by 1.
    let root_ino = resolve(&fs, "/").expect("resolve root");
    let (root, _) = fs.read_inode_verified(root_ino).expect("read root");
    assert!(
        root.links_count >= 3,
        "root nlink must be >=3 after mkdir /fresh_dir (got {})",
        root.links_count
    );

    fs::remove_file(path).ok();
}

#[test]
fn mkdir_rejects_existing_target() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    fs.apply_mkdir("/dup", 0o755).expect("first mkdir");
    let err = fs.apply_mkdir("/dup", 0o755).unwrap_err();
    assert!(format!("{err}").contains("already exists"));

    fs::remove_file(path).ok();
}

#[test]
fn mkdir_survives_remount_and_is_listable() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_mkdir("/persisted", 0o700).expect("mkdir");
    }

    // Remount RO and walk the parent's directory entries — the new dir
    // must appear.
    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/persisted").expect("lookup after remount");
    let (inode, _) = fs.read_inode_verified(ino).expect("read");
    assert!(inode.is_dir());

    fs::remove_file(path).ok();
}

#[test]
fn rmdir_removes_empty_dir() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    let _ino = fs.apply_mkdir("/to_remove", 0o755).expect("mkdir");
    let before_root = fs
        .read_inode_verified(resolve(&fs, "/").unwrap())
        .unwrap()
        .0
        .links_count;

    fs.apply_rmdir("/to_remove").expect("rmdir");

    // Target now resolves to NotFound.
    assert!(
        resolve(&fs, "/to_remove").is_none(),
        "removed dir must not be resolvable"
    );
    // Parent nlink back to its original value (-1 from the rmdir).
    let after_root = fs
        .read_inode_verified(resolve(&fs, "/").unwrap())
        .unwrap()
        .0
        .links_count;
    assert_eq!(
        after_root,
        before_root - 1,
        "parent nlink must drop by 1 on rmdir (before={before_root}, after={after_root})"
    );

    fs::remove_file(path).ok();
}

#[test]
fn rmdir_refuses_non_empty_dir() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    fs.apply_mkdir("/parent", 0o755).expect("mkdir parent");
    fs.apply_mkdir("/parent/child", 0o755).expect("mkdir child");

    let err = fs.apply_rmdir("/parent").unwrap_err();
    assert!(
        format!("{err}").contains("not empty"),
        "unexpected error: {err}"
    );

    fs::remove_file(path).ok();
}

#[test]
fn rmdir_on_regular_file_rejected() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let err = fs.apply_rmdir("/test.txt").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not a directory") || msg.contains("NotADirectory"),
        "unexpected error: {msg}"
    );
    fs::remove_file(path).ok();
}

#[test]
fn readonly_mount_rejects_mkdir_and_rmdir() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    assert!(fs.apply_mkdir("/foo", 0o755).is_err());
    assert!(fs.apply_rmdir("/foo").is_err());
    fs::remove_file(path).ok();
}
