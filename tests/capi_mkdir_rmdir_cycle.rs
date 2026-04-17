//! End-to-end mkdir + rmdir cycle tests through the C ABI.
//!
//! Verifies the happy path: mkdir creates a directory that's enumerable,
//! rmdir removes it, and state persists across remounts.

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const SRC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test-disks/ext4-basic.img"
);

fn scratch(label: &str) -> PathBuf {
    static C: AtomicU32 = AtomicU32::new(0);
    let n = C.fetch_add(1, Ordering::Relaxed);
    let dst = PathBuf::from(format!(
        "/tmp/ext4rs_capi_mkrmdir_{label}_{}_{n}.img",
        std::process::id()
    ));
    let mut out = fs::File::create(&dst).unwrap();
    out.write_all(&fs::read(SRC).unwrap()).unwrap();
    dst
}

fn last_err() -> String {
    unsafe {
        CStr::from_ptr(ext4rs_last_error()).to_string_lossy().into_owned()
    }
}

fn enumerate_root(fs_h: *mut ext4rs_fs_t) -> Vec<String> {
    let root = CString::new("/").unwrap();
    let iter = unsafe { ext4rs_dir_open(fs_h, root.as_ptr()) };
    assert!(!iter.is_null());
    let mut names = Vec::new();
    loop {
        let e = unsafe { ext4rs_dir_next(iter) };
        if e.is_null() { break; }
        let ent = unsafe { &*e };
        let b: Vec<u8> = ent.name[..ent.name_len as usize].iter().map(|b| *b as u8).collect();
        names.push(String::from_utf8_lossy(&b).into_owned());
    }
    unsafe { ext4rs_dir_close(iter) };
    names
}

#[test]
fn mkdir_creates_an_enumerable_directory() {
    let img = scratch("mkdir_enum");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let new_dir = CString::new("/mynewdir").unwrap();

    let ino = {
        let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
        assert!(!fs_h.is_null());
        let ino = unsafe { ext4rs_mkdir(fs_h, new_dir.as_ptr(), 0o755) };
        assert_ne!(ino, 0, "mkdir failed: {}", last_err());

        // Dir should be visible in root.
        let names = enumerate_root(fs_h);
        assert!(names.contains(&"mynewdir".into()), "mkdir not in root: {names:?}");

        // New dir itself should be enumerable (at least . and ..).
        let iter = unsafe { ext4rs_dir_open(fs_h, new_dir.as_ptr()) };
        assert!(!iter.is_null());
        let mut count = 0;
        loop {
            let e = unsafe { ext4rs_dir_next(iter) };
            if e.is_null() { break; }
            count += 1;
        }
        unsafe { ext4rs_dir_close(iter) };
        assert!(count >= 2, "new dir must have . and ..");

        unsafe { ext4rs_umount(fs_h) };
        ino
    };

    // Remount ro and verify persistence + csum validity.
    {
        let fs_h = unsafe { ext4rs_mount(img_c.as_ptr()) };
        assert!(!fs_h.is_null(), "remount after mkdir: {}", last_err());

        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs_h, new_dir.as_ptr(), &mut attr) };
        assert_eq!(rc, 0, "stat new dir: {}", last_err());
        assert_eq!(attr.inode, ino);
        assert!(matches!(attr.file_type, ext4rs_file_type_t::Dir));

        unsafe { ext4rs_umount(fs_h) };
    }

    let _ = fs::remove_file(&img);
}

#[test]
fn mkdir_rmdir_cycle_leaves_root_clean() {
    let img = scratch("cycle");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let new_dir = CString::new("/ephemeral").unwrap();

    {
        let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
        assert!(!fs_h.is_null());
        // Capture baseline root listing.
        let baseline: std::collections::BTreeSet<String> =
            enumerate_root(fs_h).into_iter().collect();

        let ino = unsafe { ext4rs_mkdir(fs_h, new_dir.as_ptr(), 0o755) };
        assert_ne!(ino, 0);
        let rc = unsafe { ext4rs_rmdir(fs_h, new_dir.as_ptr()) };
        assert_eq!(rc, 0, "rmdir after mkdir: {}", last_err());

        // Root listing should now match baseline exactly.
        let after: std::collections::BTreeSet<String> =
            enumerate_root(fs_h).into_iter().collect();
        assert_eq!(after, baseline, "mkdir+rmdir cycle should leave root identical");

        unsafe { ext4rs_umount(fs_h) };
    }

    let _ = fs::remove_file(&img);
}

#[test]
fn create_then_read_back_inode() {
    let img = scratch("create_read");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path = CString::new("/fresh.txt").unwrap();

    let new_ino = {
        let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
        assert!(!fs_h.is_null());
        let ino = unsafe { ext4rs_create(fs_h, path.as_ptr(), 0o644) };
        assert_ne!(ino, 0, "create: {}", last_err());

        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs_h, path.as_ptr(), &mut attr) };
        assert_eq!(rc, 0);
        assert_eq!(attr.inode, ino);
        assert!(matches!(attr.file_type, ext4rs_file_type_t::RegFile));
        assert_eq!(attr.size, 0, "fresh file starts empty");

        unsafe { ext4rs_umount(fs_h) };
        ino
    };

    // Remount ro and verify the create persists and passes all csums.
    {
        let fs_h = unsafe { ext4rs_mount(img_c.as_ptr()) };
        assert!(!fs_h.is_null(), "remount: {}", last_err());
        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs_h, path.as_ptr(), &mut attr) };
        assert_eq!(rc, 0, "stat fresh after remount: {}", last_err());
        assert_eq!(attr.inode, new_ino);
        unsafe { ext4rs_umount(fs_h) };
    }

    let _ = fs::remove_file(&img);
}
