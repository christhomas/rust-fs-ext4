//! Path edge-case coverage for the C ABI.
//!
//! POSIX resolution rules:
//!   - "" / "/" / "//" all mean root (inode 2)
//!   - "//test.txt" equivalent to "/test.txt" (internal doubled slashes collapsed)
//!   - "/subdir/" equivalent to "/subdir" (trailing slash OK on directories)
//!   - "/test.txt/" must fail with ENOTDIR (trailing slash on non-dir is a
//!     POSIX violation — e.g. `rm /test.txt/` must not succeed even though
//!     /test.txt exists, because the path explicitly asked for a directory).

use fs_ext4::capi::*;
use std::ffi::{CStr, CString};
use std::path::Path;

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

fn last_err() -> String {
    unsafe {
        let p = fs_ext4_last_error();
        if p.is_null() {
            return String::new();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn mount_or_skip() -> Option<*mut fs_ext4_fs_t> {
    if !Path::new(IMAGE).exists() {
        return None;
    }
    let p = CString::new(IMAGE).unwrap();
    let fs = unsafe { fs_ext4_mount(p.as_ptr()) };
    if fs.is_null() {
        return None;
    }
    Some(fs)
}

fn stat_ino(fs: *mut fs_ext4_fs_t, path: &str) -> Option<u32> {
    let c = CString::new(path).unwrap();
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_stat(fs, c.as_ptr(), &mut attr) };
    if rc == 0 {
        Some(attr.inode)
    } else {
        None
    }
}

#[test]
fn empty_slash_and_double_slash_all_resolve_to_root() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let by_slash = stat_ino(fs, "/").expect("/");
    let by_empty = stat_ino(fs, "").expect("empty");
    let by_double = stat_ino(fs, "//").expect("//");
    assert_eq!(by_slash, 2, "/ should resolve to root inode 2");
    assert_eq!(by_empty, 2, "empty string should resolve to root");
    assert_eq!(by_double, 2, "// should resolve to root");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn trailing_slash_on_regular_file_yields_enotdir() {
    // POSIX: `foo/` only valid when foo is a directory. `/test.txt/` must
    // fail with ENOTDIR. Previously accepted as equivalent to `/test.txt`
    // which allowed `unlink("/test.txt/")` to succeed — bug flagged by @4.
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let ino = stat_ino(fs, "/test.txt/");
    assert!(ino.is_none(), "/test.txt/ must not resolve as a directory");
    assert_eq!(fs_ext4_last_errno(), 20, "ENOTDIR expected"); // ENOTDIR
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn doubled_slashes_in_path_are_tolerated() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let plain = stat_ino(fs, "/test.txt").expect("/test.txt");
    let doubled = stat_ino(fs, "//test.txt").expect("//test.txt");
    assert_eq!(plain, doubled);
    let tripled = stat_ino(fs, "///test.txt").expect("///test.txt");
    assert_eq!(plain, tripled);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn path_through_subdir_with_trailing_slash() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let plain = stat_ino(fs, "/subdir").expect("/subdir");
    let trailing = stat_ino(fs, "/subdir/").expect("/subdir/");
    assert_eq!(plain, trailing);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn nonexistent_component_yields_enoent() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let ino = stat_ino(fs, "/does-not-exist-at-all");
    assert!(ino.is_none());
    assert_eq!(fs_ext4_last_errno(), 2); // ENOENT
    assert!(!last_err().is_empty());
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn file_used_as_directory_mid_path_yields_enotdir() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    // /test.txt is a file; treating it as a directory must fail with ENOTDIR.
    let ino = stat_ino(fs, "/test.txt/anything");
    assert!(ino.is_none());
    assert_eq!(fs_ext4_last_errno(), 20); // ENOTDIR
    unsafe { fs_ext4_umount(fs) };
}
