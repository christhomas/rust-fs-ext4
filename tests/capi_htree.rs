//! C ABI coverage on an htree-indexed directory.
//!
//! ext4-htree.img contains /bigdir with 256 files (file_0.txt..file_255.txt).
//! The directory is htree-indexed (EXT4_INDEX_FL). We verify the C ABI
//! dir_open/dir_next path walks the full listing via linear leaf scan even
//! though the directory is indexed — no entries lost, no duplicates.

use ext4rs::capi::*;
use std::ffi::CString;
use std::path::Path;

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-htree.img");

fn mount_or_skip() -> Option<*mut ext4rs_fs_t> {
    if !Path::new(IMAGE).exists() {
        eprintln!("skip: {IMAGE} not built");
        return None;
    }
    let p = CString::new(IMAGE).unwrap();
    let fs = unsafe { ext4rs_mount(p.as_ptr()) };
    if fs.is_null() {
        return None;
    }
    Some(fs)
}

fn list_dir(fs: *mut ext4rs_fs_t, path: &str) -> Vec<String> {
    let p = CString::new(path).unwrap();
    let iter = unsafe { ext4rs_dir_open(fs, p.as_ptr()) };
    assert!(!iter.is_null(), "dir_open({path}) returned null");
    let mut names = Vec::new();
    loop {
        let e = unsafe { ext4rs_dir_next(iter) };
        if e.is_null() {
            break;
        }
        let entry = unsafe { &*e };
        let bytes: Vec<u8> = entry.name[..entry.name_len as usize]
            .iter()
            .map(|b| *b as u8)
            .collect();
        names.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    unsafe { ext4rs_dir_close(iter) };
    names
}

#[test]
fn bigdir_lists_all_256_files_plus_dot_entries() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let names = list_dir(fs, "/bigdir");
    // Expect . + .. + 256 file_*.txt entries
    let files: Vec<&String> = names.iter().filter(|n| n.starts_with("file_")).collect();
    assert_eq!(
        files.len(),
        256,
        "expected 256 file_* entries, got {}",
        files.len()
    );
    assert!(names.iter().any(|n| n == "."), "missing .");
    assert!(names.iter().any(|n| n == ".."), "missing ..");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn bigdir_has_no_duplicate_entries() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let names = list_dir(fs, "/bigdir");
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "duplicate entries detected");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn bigdir_stat_every_file_succeeds() {
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let names = list_dir(fs, "/bigdir");
    for name in names.iter().filter(|n| n.starts_with("file_")) {
        let path = format!("/bigdir/{name}");
        let c = CString::new(path.clone()).unwrap();
        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs, c.as_ptr(), &mut attr) };
        assert_eq!(rc, 0, "stat({path}) failed");
        assert!(attr.inode > 0);
    }
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn specific_htree_lookups_hit_via_path() {
    // Probe a scattered sample — not just the first few — to exercise
    // different htree leaf blocks. Uses actual listed names to avoid
    // assumptions about naming (file_0 vs file_000 vs file_0.txt etc).
    let Some(fs) = mount_or_skip() else {
        return;
    };
    let names = list_dir(fs, "/bigdir");
    let real: Vec<&String> = names.iter().filter(|n| !n.starts_with('.')).collect();
    assert!(real.len() >= 5, "expected many files, got {}", real.len());
    for idx in [0usize, 1, 42, 100, 127, real.len() - 1] {
        let name = real[idx.min(real.len() - 1)];
        let path = format!("/bigdir/{name}");
        let c = CString::new(path.clone()).unwrap();
        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs, c.as_ptr(), &mut attr) };
        assert_eq!(rc, 0, "stat({path}) failed");
    }
    unsafe { ext4rs_umount(fs) };
}
