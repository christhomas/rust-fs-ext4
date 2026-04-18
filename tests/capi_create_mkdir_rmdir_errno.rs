//! Errno semantics for the Phase 4 create/mkdir/rmdir ops.
//!
//! Complements @6's functional tests (capi_unlink.rs and any
//! capi_create/capi_mkdir tests they land) with assertions on
//! `fs_ext4_last_errno`. POSIX codes FSKit needs to surface:
//!
//!   create/mkdir on existing target  → EEXIST (17)
//!   rmdir on non-empty dir           → ENOTEMPTY (66)
//!   rmdir on a regular file          → ENOTDIR (20)
//!   rename with missing src          → ENOENT (2)
//!
//! Wired via the new Error::AlreadyExists / Error::DirectoryNotEmpty
//! variants → `to_errno()` mapping in error.rs.

use fs_ext4::capi::*;
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

fn scratch(label: &str) -> PathBuf {
    static C: AtomicU32 = AtomicU32::new(0);
    let n = C.fetch_add(1, Ordering::Relaxed);
    let dst = PathBuf::from(format!(
        "/tmp/fs_ext4_cmr_errno_{label}_{}_{n}.img",
        std::process::id()
    ));
    let mut out = fs::File::create(&dst).unwrap();
    out.write_all(&fs::read(SRC).unwrap()).unwrap();
    dst
}

#[test]
fn create_on_existing_file_sets_eexist() {
    let img = scratch("create_eexist");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { fs_ext4_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let existing = CString::new("/test.txt").unwrap();
    let ino = unsafe { fs_ext4_create(fs_h, existing.as_ptr(), 0o644) };
    assert_eq!(ino, 0, "create on existing must fail");
    assert_eq!(fs_ext4_last_errno(), 17, "EEXIST");

    unsafe { fs_ext4_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn mkdir_on_existing_dir_sets_eexist() {
    let img = scratch("mkdir_eexist");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { fs_ext4_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let existing = CString::new("/subdir").unwrap();
    let ino = unsafe { fs_ext4_mkdir(fs_h, existing.as_ptr(), 0o755) };
    assert_eq!(ino, 0, "mkdir on existing must fail");
    assert_eq!(fs_ext4_last_errno(), 17, "EEXIST");

    unsafe { fs_ext4_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn rmdir_on_nonempty_dir_sets_enotempty() {
    // /subdir in ext4-basic.img has entries beyond . and ..
    let img = scratch("rmdir_notempty");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { fs_ext4_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let dir = CString::new("/subdir").unwrap();
    let rc = unsafe { fs_ext4_rmdir(fs_h, dir.as_ptr()) };
    assert_eq!(rc, -1);
    assert_eq!(fs_ext4_last_errno(), 66, "ENOTEMPTY (macOS)");

    unsafe { fs_ext4_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn rmdir_on_regular_file_sets_enotdir() {
    let img = scratch("rmdir_file");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { fs_ext4_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let file = CString::new("/test.txt").unwrap();
    let rc = unsafe { fs_ext4_rmdir(fs_h, file.as_ptr()) };
    assert_eq!(rc, -1);
    assert_eq!(fs_ext4_last_errno(), 20, "ENOTDIR");

    unsafe { fs_ext4_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn rename_missing_source_sets_enoent() {
    let img = scratch("rename_enoent");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { fs_ext4_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let src = CString::new("/does-not-exist.txt").unwrap();
    let dst = CString::new("/whatever.txt").unwrap();
    let rc = unsafe { fs_ext4_rename(fs_h, src.as_ptr(), dst.as_ptr()) };
    assert_eq!(rc, -1);
    assert_eq!(fs_ext4_last_errno(), 2, "ENOENT");

    unsafe { fs_ext4_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn create_null_args_set_einval() {
    let ino = unsafe { fs_ext4_create(std::ptr::null_mut(), std::ptr::null(), 0o644) };
    assert_eq!(ino, 0);
    assert_eq!(fs_ext4_last_errno(), 22);
}

#[test]
fn mkdir_null_args_set_einval() {
    let ino = unsafe { fs_ext4_mkdir(std::ptr::null_mut(), std::ptr::null(), 0o755) };
    assert_eq!(ino, 0);
    assert_eq!(fs_ext4_last_errno(), 22);
}

#[test]
fn rmdir_null_args_set_einval() {
    let rc = unsafe { fs_ext4_rmdir(std::ptr::null_mut(), std::ptr::null()) };
    assert_eq!(rc, -1);
    assert_eq!(fs_ext4_last_errno(), 22);
}
