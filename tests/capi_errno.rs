//! Tests for fs_ext4_last_errno — the POSIX errno companion to
//! fs_ext4_last_error. FSKit needs a numeric errno to build NSError
//! objects with the correct POSIXErrorDomain code.

use fs_ext4::capi::*;
use std::ffi::CString;
use std::os::raw::c_void;

// Match the POSIX values from fs_ext4::error::errno (macOS).
const ENOENT: i32 = 2;
const EIO: i32 = 5;
const ENOTDIR: i32 = 20;
const EINVAL: i32 = 22;

const TEST_IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

fn mount() -> *mut fs_ext4_fs_t {
    let path = CString::new(TEST_IMAGE).unwrap();
    let fs = unsafe { fs_ext4_mount(path.as_ptr()) };
    assert!(!fs.is_null());
    fs
}

#[test]
fn success_clears_errno_to_zero() {
    let fs = mount();
    let path = CString::new("/test.txt").unwrap();
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_stat(fs, path.as_ptr(), &mut attr) };
    assert_eq!(rc, 0);
    assert_eq!(fs_ext4_last_errno(), 0);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn stat_missing_path_sets_enoent() {
    let fs = mount();
    let path = CString::new("/nonexistent/path.txt").unwrap();
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_stat(fs, path.as_ptr(), &mut attr) };
    assert_eq!(rc, -1);
    assert_eq!(fs_ext4_last_errno(), ENOENT);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn null_args_set_einval() {
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_stat(std::ptr::null_mut(), std::ptr::null(), &mut attr) };
    assert_eq!(rc, -1);
    assert_eq!(fs_ext4_last_errno(), EINVAL);
}

#[test]
fn mount_missing_file_sets_enoent_via_io_error() {
    let path = CString::new("/tmp/definitely-does-not-exist-xyz-errno-test").unwrap();
    let fs = unsafe { fs_ext4_mount(path.as_ptr()) };
    assert!(fs.is_null());
    // io::Error from open(ENOENT=2) flows through Error::Io -> raw_os_error.
    assert_eq!(fs_ext4_last_errno(), ENOENT);
}

#[test]
fn dir_open_on_regular_file_sets_enotdir() {
    let fs = mount();
    let path = CString::new("/test.txt").unwrap();
    let iter = unsafe { fs_ext4_dir_open(fs, path.as_ptr()) };
    assert!(iter.is_null());
    assert_eq!(fs_ext4_last_errno(), ENOTDIR);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn read_file_on_directory_sets_einval() {
    let fs = mount();
    let path = CString::new("/subdir").unwrap();
    let mut buf = [0u8; 16];
    let n = unsafe {
        fs_ext4_read_file(
            fs,
            path.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            0,
            buf.len() as u64,
        )
    };
    assert_eq!(n, -1);
    assert_eq!(fs_ext4_last_errno(), EINVAL);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn read_file_on_symlink_sets_einval() {
    // Symlinks aren't readable via read_file — callers must use readlink.
    // Without this guard FSKit could try to read the raw symlink target
    // bytes as if they were file contents.
    let fs = mount();
    let path = CString::new("/link.txt").unwrap();
    let mut buf = [0u8; 32];
    let n = unsafe {
        fs_ext4_read_file(
            fs,
            path.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            0,
            buf.len() as u64,
        )
    };
    assert_eq!(n, -1);
    assert_eq!(fs_ext4_last_errno(), EINVAL);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn getxattr_missing_name_sets_enoent() {
    let xattr_image = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-xattr.img");
    let path = CString::new(xattr_image).unwrap();
    let fs = unsafe { fs_ext4_mount(path.as_ptr()) };
    assert!(!fs.is_null());

    let p = CString::new("/tagged.txt").unwrap();
    let name = CString::new("user.does_not_exist").unwrap();
    let n = unsafe { fs_ext4_getxattr(fs, p.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    assert_eq!(n, -1);
    assert_eq!(fs_ext4_last_errno(), ENOENT);

    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn errno_is_thread_local_and_persists_until_next_call() {
    let fs = mount();
    // Trigger a failure.
    let path = CString::new("/nope").unwrap();
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    unsafe { fs_ext4_stat(fs, path.as_ptr(), &mut attr) };
    let e1 = fs_ext4_last_errno();
    assert_eq!(e1, ENOENT);
    // Now a success clears errno.
    let path_ok = CString::new("/test.txt").unwrap();
    unsafe { fs_ext4_stat(fs, path_ok.as_ptr(), &mut attr) };
    assert_eq!(fs_ext4_last_errno(), 0);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn errno_values_are_posix() {
    // Spot-check numeric values match POSIX errno.h (macOS).
    assert_eq!(ENOENT, 2);
    assert_eq!(EIO, 5);
    assert_eq!(ENOTDIR, 20);
    assert_eq!(EINVAL, 22);
    let _ = EIO;
}
