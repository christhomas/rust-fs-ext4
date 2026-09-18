//! Stress-ish smoke test against ext4-manyfiles.img via the C ABI.
//!
//! Exercises the dir_open/dir_next path on a larger, htree-indexed directory
//! than ext4-basic.img covers. Verifies no regression in iteration when the
//! directory spans many blocks.

use fs_ext4::capi::*;
use std::ffi::{CStr, CString};

const IMAGE: &str = "ext4-manyfiles.img";

fn mount_fixture() -> *mut fs_ext4_fs_t {
    let path = fs_ext4_test_support::fixture(env!("CARGO_MANIFEST_DIR"), IMAGE);
    let p = CString::new(path.as_str()).unwrap();
    let fs = unsafe { fs_ext4_mount(p.as_ptr()) };
    assert!(
        !fs.is_null(),
        "fs_ext4_mount({path}) failed: {}",
        unsafe { std::ffi::CStr::from_ptr(fs_ext4_last_error()) }.to_string_lossy()
    );
    fs
}

fn list_dir(fs: *mut fs_ext4_fs_t, path: &str) -> Vec<String> {
    let p = CString::new(path).unwrap();
    let iter = unsafe { fs_ext4_dir_open(fs, p.as_ptr()) };
    assert!(!iter.is_null(), "dir_open failed on {path}");
    let mut names = Vec::new();
    loop {
        let e = unsafe { fs_ext4_dir_next(iter) };
        if e.is_null() {
            break;
        }
        let entry = unsafe { &*e };
        let name_len = entry.name_len as usize;
        let bytes: Vec<u8> = entry.name[..name_len]
            .iter()
            .map(|b| b.to_ne_bytes()[0])
            .collect();
        names.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    unsafe { fs_ext4_dir_close(iter) };
    names
}

#[test]
fn mount_and_umount_manyfiles() {
    let fs = mount_fixture();
    let mut info: fs_ext4_volume_info_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_get_volume_info(fs, &mut info) };
    assert_eq!(rc, 0, "get_volume_info failed");
    assert!(info.block_size >= 1024);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn root_listing_includes_dot_and_dotdot() {
    let fs = mount_fixture();
    let entries = list_dir(fs, "/");
    assert!(entries.iter().any(|n| n == "."), "missing . in root");
    assert!(entries.iter().any(|n| n == ".."), "missing .. in root");
    eprintln!("root has {} entries", entries.len());
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn stat_works_on_every_root_entry() {
    let fs = mount_fixture();
    let entries = list_dir(fs, "/");
    let mut errors = 0;
    for name in &entries {
        if name == "." || name == ".." {
            continue;
        }
        let path = format!("/{name}");
        let c = CString::new(path.clone()).unwrap();
        let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { fs_ext4_stat(fs, c.as_ptr(), &mut attr) };
        if rc != 0 {
            errors += 1;
            let err = unsafe {
                CStr::from_ptr(fs_ext4_last_error())
                    .to_string_lossy()
                    .into_owned()
            };
            eprintln!("stat({path}) failed: {err}");
        } else {
            assert!(attr.inode > 0);
        }
    }
    assert_eq!(errors, 0, "some entries failed to stat");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn listing_does_not_panic_on_large_dir() {
    let fs = mount_fixture();
    // Whatever the biggest directory turns out to be, opening + draining it
    // must not panic or OOM. 64MB image caps this at a reasonable size.
    let entries = list_dir(fs, "/");
    assert!(entries.len() >= 2, "at minimum we should see . and ..");
    unsafe { fs_ext4_umount(fs) };
}
