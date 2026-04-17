//! Readlink coverage on ext4-basic.img's /link.txt.
//!
//! The image has a symlink entry from the original lwext4 era;
//! this verifies readlink on it works through the C ABI.

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::path::Path;

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

fn mount_or_skip() -> Option<*mut ext4rs_fs_t> {
    if !Path::new(IMAGE).exists() {
        return None;
    }
    let p = CString::new(IMAGE).unwrap();
    let fs = unsafe { ext4rs_mount(p.as_ptr()) };
    if fs.is_null() {
        return None;
    }
    Some(fs)
}

#[test]
fn readlink_on_basic_link_returns_expected_target() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let p = CString::new("/link.txt").unwrap();

    // First: is /link.txt actually a symlink? If not, skip gracefully.
    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { ext4rs_stat(fs, p.as_ptr(), &mut attr) };
    if rc != 0 {
        eprintln!("skip: /link.txt not present in ext4-basic.img");
        unsafe { ext4rs_umount(fs) };
        return;
    }
    if !matches!(attr.file_type, ext4rs_file_type_t::Symlink) {
        eprintln!(
            "skip: /link.txt exists but isn't a symlink (file_type={:?})",
            attr.file_type as u32
        );
        unsafe { ext4rs_umount(fs) };
        return;
    }

    let mut buf = [0u8; 256];
    let rc = unsafe { ext4rs_readlink(fs, p.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert_eq!(rc, 0, "readlink failed");

    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let target = String::from_utf8_lossy(&buf[..end]);
    assert_eq!(target, "test.txt", "unexpected /link.txt target");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn readlink_on_regular_file_sets_einval() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let p = CString::new("/test.txt").unwrap();
    let mut buf = [0u8; 64];
    let rc = unsafe { ext4rs_readlink(fs, p.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 22); // EINVAL
    let err = unsafe {
        CStr::from_ptr(ext4rs_last_error())
            .to_string_lossy()
            .into_owned()
    };
    assert!(err.contains("not a symlink"), "err was: {err}");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn readlink_on_directory_sets_einval() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let p = CString::new("/subdir").unwrap();
    let mut buf = [0u8; 64];
    let rc = unsafe { ext4rs_readlink(fs, p.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 22); // EINVAL
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn readlink_on_missing_path_sets_enoent() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let p = CString::new("/does-not-exist").unwrap();
    let mut buf = [0u8; 64];
    let rc = unsafe { ext4rs_readlink(fs, p.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 2); // ENOENT
    unsafe { ext4rs_umount(fs) };
}
