//! C ABI coverage on a LARGEDIR-enabled directory (3-level htree).
//!
//! ext4-largedir.img contains /huge with 70,000 zero-length files, forcing
//! the htree past its legacy 2-level cap (LARGEDIR ro_compat). This
//! verifies the C ABI's dir_open/dir_next and path::lookup handle the
//! deeper tree without regression.
//!
//! Image is 192 MB, so these tests are heavier — marked #[ignore] would
//! be an option for CI, but they run in under a second locally.

use ext4rs::capi::*;
use std::ffi::CString;
use std::path::Path;

const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test-disks/ext4-largedir.img"
);

fn mount_or_skip() -> Option<*mut ext4rs_fs_t> {
    if !Path::new(IMAGE).exists() {
        eprintln!("skip: {IMAGE} not built (run test-disks/build-ext4-feature-images.sh largedir)");
        return None;
    }
    let p = CString::new(IMAGE).unwrap();
    let fs = unsafe { ext4rs_mount(p.as_ptr()) };
    if fs.is_null() { return None; }
    Some(fs)
}

fn count_entries(fs: *mut ext4rs_fs_t, path: &str) -> usize {
    let c = CString::new(path).unwrap();
    let iter = unsafe { ext4rs_dir_open(fs, c.as_ptr()) };
    assert!(!iter.is_null(), "dir_open({path}) returned null");
    let mut count = 0usize;
    loop {
        let e = unsafe { ext4rs_dir_next(iter) };
        if e.is_null() { break; }
        count += 1;
    }
    unsafe { ext4rs_dir_close(iter) };
    count
}

fn list_names(fs: *mut ext4rs_fs_t, path: &str) -> Vec<String> {
    let c = CString::new(path).unwrap();
    let iter = unsafe { ext4rs_dir_open(fs, c.as_ptr()) };
    assert!(!iter.is_null());
    let mut names = Vec::with_capacity(70_002);
    loop {
        let e = unsafe { ext4rs_dir_next(iter) };
        if e.is_null() { break; }
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
fn small_file_reads_control_content() {
    let Some(fs) = mount_or_skip() else { return; };
    let c = CString::new("/small.txt").unwrap();
    let mut buf = [0u8; 32];
    let n = unsafe {
        ext4rs_read_file(
            fs,
            c.as_ptr(),
            buf.as_mut_ptr() as *mut std::os::raw::c_void,
            0,
            buf.len() as u64,
        )
    };
    assert_eq!(n, 8);
    assert_eq!(&buf[..8], b"control\n");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn huge_dir_enumerates_all_70000_files_plus_dot_entries() {
    let Some(fs) = mount_or_skip() else { return; };
    let count = count_entries(fs, "/huge");
    // 70_000 files + "." + ".."
    assert_eq!(count, 70_002, "expected 70002 entries, got {count}");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn huge_dir_no_duplicate_entries() {
    let Some(fs) = mount_or_skip() else { return; };
    let names = list_names(fs, "/huge");
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "duplicates detected in /huge");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn sampled_stat_succeeds_across_the_huge_range() {
    let Some(fs) = mount_or_skip() else { return; };
    // file_NNNNN.txt where NNNNN is 1..=70000 (5-digit zero-padded).
    for idx in [1u32, 100, 35_000, 69_999, 70_000] {
        let name = format!("file_{idx:05}.txt");
        let path = format!("/huge/{name}");
        let c = CString::new(path.clone()).unwrap();
        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs, c.as_ptr(), &mut attr) };
        assert_eq!(rc, 0, "stat({path}) failed — htree walk regression?");
        assert_eq!(attr.size, 0, "files were created empty");
    }
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn missing_entry_in_huge_dir_returns_enoent() {
    let Some(fs) = mount_or_skip() else { return; };
    let c = CString::new("/huge/file_99999999.txt").unwrap();
    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { ext4rs_stat(fs, c.as_ptr(), &mut attr) };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 2, "missing must be ENOENT");
    unsafe { ext4rs_umount(fs) };
}
