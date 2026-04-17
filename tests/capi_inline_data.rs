//! C ABI read-through test for files using INCOMPAT_INLINE_DATA.
//!
//! ext4-inline.img layout (built by test-disks/build-ext4-feature-images.sh):
//!   /tiny.txt   — "tiny inline\n" (12 bytes, fits in i_block alone)
//!   /medium.txt — 100x 'A' (overflows into system.data xattr)
//!   /symlink    — symlink to "target/path/here"
//!
//! Before the inline_data wiring in capi.rs, ext4rs_read_file returned 0
//! for any file with INLINE_DATA_FL set. These tests lock in the fix.

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;

const TEST_IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-inline.img");

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

#[test]
fn reads_tiny_inline_file_full_content() {
    let fs = mount();
    let path = CString::new("/tiny.txt").unwrap();

    let mut buf = [0u8; 64];
    let n = unsafe {
        ext4rs_read_file(
            fs,
            path.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            0,
            buf.len() as u64,
        )
    };
    assert_eq!(n, 12, "tiny.txt should be 12 bytes: {}", last_err_str());
    assert_eq!(&buf[..12], b"tiny inline\n");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn reads_medium_inline_file_with_xattr_overflow() {
    let fs = mount();
    let path = CString::new("/medium.txt").unwrap();

    let mut buf = [0u8; 128];
    let n = unsafe {
        ext4rs_read_file(
            fs,
            path.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            0,
            buf.len() as u64,
        )
    };
    assert_eq!(
        n,
        100,
        "medium.txt should be 100 bytes (inline+xattr): {}",
        last_err_str()
    );
    assert!(buf[..100].iter().all(|&b| b == b'A'), "content mismatch");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn inline_read_respects_offset_and_length() {
    let fs = mount();
    let path = CString::new("/medium.txt").unwrap();

    let mut buf = [0u8; 32];
    let n = unsafe { ext4rs_read_file(fs, path.as_ptr(), buf.as_mut_ptr() as *mut c_void, 50, 10) };
    assert_eq!(n, 10);
    assert!(buf[..10].iter().all(|&b| b == b'A'));

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn inline_read_past_eof_returns_zero() {
    let fs = mount();
    let path = CString::new("/tiny.txt").unwrap();

    let mut buf = [0u8; 16];
    let n = unsafe {
        ext4rs_read_file(
            fs,
            path.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            1000,
            buf.len() as u64,
        )
    };
    assert_eq!(n, 0, "reading past EOF should return 0 bytes");

    unsafe { ext4rs_umount(fs) };
}

#[test]
fn inline_data_symlink_readlink() {
    let fs = mount();
    let path = CString::new("/symlink").unwrap();

    let mut buf = [0u8; 128];
    let rc = unsafe { ext4rs_readlink(fs, path.as_ptr(), buf.as_mut_ptr() as *mut i8, buf.len()) };
    assert_eq!(rc, 0, "readlink failed: {}", last_err_str());
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    assert_eq!(&buf[..end], b"target/path/here");

    unsafe { ext4rs_umount(fs) };
}
