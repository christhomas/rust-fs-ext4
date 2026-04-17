//! C ABI smoke tests for ext4rs_listxattr + ext4rs_getxattr.
//!
//! Image layout (built by test-disks/build-ext4-feature-images.sh):
//!   /tagged.txt    user.color=red, user.com.apple.FinderInfo=<4 raw bytes>
//!   /tagged_dir    user.purpose=documents
//!   /plain.txt     (no xattrs)

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;

const TEST_IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-xattr.img");

fn last_err_str() -> String {
    unsafe {
        let p = ext4rs_last_error();
        if p.is_null() {
            return "<null>".into();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn mount() -> *mut ext4rs_fs_t {
    let path = CString::new(TEST_IMAGE).unwrap();
    let fs = unsafe { ext4rs_mount(path.as_ptr()) };
    assert!(!fs.is_null(), "mount failed: {}", last_err_str());
    fs
}

fn parse_nul_names(buf: &[u8]) -> Vec<String> {
    buf.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[test]
fn listxattr_probe_returns_required_size() {
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();

    let required = unsafe { ext4rs_listxattr(fs, path.as_ptr(), std::ptr::null_mut(), 0) };
    // Image is built with inline_data, so an implicit system.data xattr
    // (12 bytes) is present alongside the two user.* xattrs:
    //   "user.color\0" (11) + "user.com.apple.FinderInfo\0" (26) + "system.data\0" (12) = 49.
    assert!(required >= 37, "expected at least 37 bytes, got {required}");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn listxattr_writes_names_nul_separated() {
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();

    let mut buf = vec![0u8; 256];
    let ret =
        unsafe { ext4rs_listxattr(fs, path.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert!(ret > 0, "listxattr failed: {}", last_err_str());

    let names = parse_nul_names(&buf[..ret as usize]);
    assert!(
        names.contains(&"user.color".into()),
        "missing user.color: {names:?}"
    );
    assert!(
        names.contains(&"user.com.apple.FinderInfo".into()),
        "missing Finder xattr: {names:?}"
    );

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn listxattr_plain_file_has_no_user_xattrs() {
    let fs = mount();
    let path = CString::new("/plain.txt").unwrap();

    let mut buf = vec![0u8; 256];
    let ret =
        unsafe { ext4rs_listxattr(fs, path.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert!(ret >= 0, "listxattr failed: {}", last_err_str());

    let names = parse_nul_names(&buf[..ret as usize]);
    let user: Vec<_> = names.iter().filter(|n| n.starts_with("user.")).collect();
    assert!(
        user.is_empty(),
        "plain.txt should have no user.* xattrs: {user:?}"
    );

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn getxattr_probe_returns_value_size() {
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();
    let name = CString::new("user.color").unwrap();

    let size =
        unsafe { ext4rs_getxattr(fs, path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    assert_eq!(size, 3, "user.color = \"red\" should be 3 bytes");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn getxattr_writes_value_bytes() {
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();
    let name = CString::new("user.color").unwrap();

    let mut buf = [0u8; 16];
    let size = unsafe {
        ext4rs_getxattr(
            fs,
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
        )
    };
    assert_eq!(size, 3);
    assert_eq!(&buf[..3], b"red");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn getxattr_binary_value_roundtrips() {
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();
    let name = CString::new("user.com.apple.FinderInfo").unwrap();

    let mut buf = [0u8; 32];
    let size = unsafe {
        ext4rs_getxattr(
            fs,
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
        )
    };
    assert_eq!(size, 4, "FinderInfo fixture is 4 bytes");
    assert_eq!(&buf[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn getxattr_missing_returns_minus_one() {
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();
    let name = CString::new("user.does_not_exist").unwrap();

    let size =
        unsafe { ext4rs_getxattr(fs, path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    assert_eq!(size, -1, "missing xattr should return -1");
    let err = last_err_str();
    assert!(err.contains("not found"), "err was: {err}");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn listxattr_directory_has_xattrs() {
    let fs = mount();
    let path = CString::new("/tagged_dir").unwrap();

    let mut buf = vec![0u8; 256];
    let ret =
        unsafe { ext4rs_listxattr(fs, path.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert!(ret > 0, "listxattr on dir failed: {}", last_err_str());

    let names = parse_nul_names(&buf[..ret as usize]);
    assert!(
        names.contains(&"user.purpose".into()),
        "missing user.purpose on tagged_dir: {names:?}"
    );

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn getxattr_zero_length_value_returns_zero_not_minus_one() {
    // ext4-xattr.img is built with -O inline_data, so every inode carries an
    // implicit `system.data` xattr whose value is empty (0 bytes). Requesting
    // it must return 0 (a valid present-but-empty result), NOT -1 (missing).
    // This is the classic C-ABI distinction between "empty" and "absent".
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();
    let name = CString::new("system.data").unwrap();

    let sz = unsafe { ext4rs_getxattr(fs, path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    assert_eq!(
        sz, 0,
        "system.data should exist with 0-byte value, got {sz}"
    );
    assert_eq!(ext4rs_last_errno(), 0);

    // Also verify it still behaves correctly when we provide a buffer.
    let mut buf = [0u8; 16];
    let sz2 = unsafe {
        ext4rs_getxattr(
            fs,
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
        )
    };
    assert_eq!(sz2, 0);

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn listxattr_undersized_buf_still_reports_required() {
    let fs = mount();
    let path = CString::new("/tagged.txt").unwrap();

    let mut tiny = [0u8; 4];
    let required =
        unsafe { ext4rs_listxattr(fs, path.as_ptr(), tiny.as_mut_ptr() as *mut i8, tiny.len()) };
    // Even with a tiny buffer, return value is the full required size so
    // callers can re-allocate and retry.
    assert!(required >= 37, "expected >=37, got {required}");

    unsafe { ext4rs_umount(fs) };
}
