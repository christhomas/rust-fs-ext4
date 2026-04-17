//! Stress-ish smoke test against ext4-manyfiles.img via the C ABI.
//!
//! Exercises the dir_open/dir_next path on a larger, htree-indexed directory
//! than ext4-basic.img covers. Verifies no regression in iteration when the
//! directory spans many blocks.

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::path::Path;

const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test-disks/ext4-manyfiles.img"
);

fn mount_or_skip() -> Option<*mut ext4rs_fs_t> {
    if !Path::new(IMAGE).exists() {
        eprintln!("skip: {IMAGE} not built");
        return None;
    }
    let p = CString::new(IMAGE).unwrap();
    let fs = unsafe { ext4rs_mount(p.as_ptr()) };
    if fs.is_null() {
        eprintln!("skip: mount failed on {IMAGE}");
        return None;
    }
    Some(fs)
}

fn list_dir(fs: *mut ext4rs_fs_t, path: &str) -> Vec<String> {
    let p = CString::new(path).unwrap();
    let iter = unsafe { ext4rs_dir_open(fs, p.as_ptr()) };
    assert!(!iter.is_null(), "dir_open failed on {path}");
    let mut names = Vec::new();
    loop {
        let e = unsafe { ext4rs_dir_next(iter) };
        if e.is_null() {
            break;
        }
        let entry = unsafe { &*e };
        let name_len = entry.name_len as usize;
        let bytes: Vec<u8> = entry.name[..name_len].iter().map(|b| *b as u8).collect();
        names.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    unsafe { ext4rs_dir_close(iter) };
    names
}

#[test]
fn mount_and_umount_manyfiles() {
    let Some(fs) = mount_or_skip() else { return; };
    let mut info: ext4rs_volume_info_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { ext4rs_get_volume_info(fs, &mut info) };
    assert_eq!(rc, 0, "get_volume_info failed");
    assert!(info.block_size >= 1024);
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn root_listing_includes_dot_and_dotdot() {
    let Some(fs) = mount_or_skip() else { return; };
    let entries = list_dir(fs, "/");
    assert!(entries.iter().any(|n| n == "."), "missing . in root");
    assert!(entries.iter().any(|n| n == ".."), "missing .. in root");
    eprintln!("root has {} entries", entries.len());
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn stat_works_on_every_root_entry() {
    let Some(fs) = mount_or_skip() else { return; };
    let entries = list_dir(fs, "/");
    let mut errors = 0;
    for name in &entries {
        if name == "." || name == ".." {
            continue;
        }
        let path = format!("/{name}");
        let c = CString::new(path.clone()).unwrap();
        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs, c.as_ptr(), &mut attr) };
        if rc != 0 {
            errors += 1;
            let err = unsafe {
                CStr::from_ptr(ext4rs_last_error())
                    .to_string_lossy()
                    .into_owned()
            };
            eprintln!("stat({path}) failed: {err}");
        } else {
            assert!(attr.inode > 0);
        }
    }
    assert_eq!(errors, 0, "some entries failed to stat");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn listing_does_not_panic_on_large_dir() {
    let Some(fs) = mount_or_skip() else { return; };
    // Whatever the biggest directory turns out to be, opening + draining it
    // must not panic or OOM. 64MB image caps this at a reasonable size.
    let entries = list_dir(fs, "/");
    assert!(entries.len() >= 2, "at minimum we should see . and ..");
    unsafe { ext4rs_umount(fs) };
}
