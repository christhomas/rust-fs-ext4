//! Truncate type-check regression.
//!
//! SERIOUS BUG found against @3 ext4rs_truncate: truncate(/dir, 0)
//! silently succeeds, zeros the directory's size, and frees its data
//! blocks — leaving the filesystem in a corrupted state where the dir
//! still exists as an inode but has lost all entries (including . and
//! ..). Finder calling truncate via an FSKit op could therefore destroy
//! a directory's entire subtree without error.
//!
//! POSIX ftruncate(2): "If fildes refers to a directory, ftruncate()
//! shall fail with EISDIR."
//!
//! Current capi.rs calls resolve_path → apply_truncate_shrink without
//! checking `inode.is_file()` first. The fix is to gate on file type
//! before calling apply_truncate_shrink, mirroring the guard in
//! ext4rs_read_file (which returns EINVAL for non-files).

use ext4rs::capi::*;
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

fn scratch() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dst = PathBuf::from(format!(
        "/tmp/ext4rs_capi_trunc_type_{}_{n}.img",
        std::process::id()
    ));
    let mut out = fs::File::create(&dst).unwrap();
    out.write_all(&fs::read(SRC).unwrap()).unwrap();
    dst
}

#[test]
fn truncate_on_directory_should_fail_with_eisdir() {
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let dir = CString::new("/subdir").unwrap();
    let rc = unsafe { ext4rs_truncate(fs_h, dir.as_ptr(), 0) };
    assert_eq!(rc, -1, "truncate on directory MUST fail");
    // POSIX EISDIR = 21 on Linux/macOS. Also accept EINVAL (22) as our
    // general type-mismatch code.
    let e = ext4rs_last_errno();
    assert!(
        e == 21 || e == 22,
        "expected EISDIR (21) or EINVAL (22), got {e}"
    );

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn truncate_grow_fails_with_einval() {
    // Growing a file with truncate isn't supported. The apply layer returns
    // Error::Corrupt which would map to EIO; the capi wrapper should
    // surface EINVAL instead so FSKit callers get a meaningful error.
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let path = CString::new("/test.txt").unwrap();
    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    unsafe { ext4rs_stat(fs_h, path.as_ptr(), &mut attr) };
    let original = attr.size;

    let rc = unsafe { ext4rs_truncate(fs_h, path.as_ptr(), original + 4096) };
    assert_eq!(rc, -1);
    assert_eq!(
        ext4rs_last_errno(),
        22,
        "expected EINVAL for grow, got {}",
        ext4rs_last_errno()
    );

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn truncate_on_symlink_fails_with_einval() {
    // link.txt is a symlink (→ test.txt). Truncating a symlink also isn't
    // allowed per POSIX; our guard returns EINVAL for any non-file non-dir.
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let link = CString::new("/link.txt").unwrap();
    let rc = unsafe { ext4rs_truncate(fs_h, link.as_ptr(), 0) };
    assert_eq!(rc, -1, "truncate on symlink MUST fail");
    assert_eq!(ext4rs_last_errno(), 22, "expected EINVAL for symlink");

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn truncate_on_directory_leaves_the_dir_intact() {
    // Regression for the corruption bug: confirm that after a rejected
    // truncate the directory is still enumerable (still has . and ..).
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let dir = CString::new("/subdir").unwrap();
    let _ = unsafe { ext4rs_truncate(fs_h, dir.as_ptr(), 0) };

    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    unsafe { ext4rs_stat(fs_h, dir.as_ptr(), &mut attr) };
    assert!(attr.size > 0, "directory size must not be zeroed");

    let iter = unsafe { ext4rs_dir_open(fs_h, dir.as_ptr()) };
    assert!(!iter.is_null());
    let mut count = 0;
    loop {
        let e = unsafe { ext4rs_dir_next(iter) };
        if e.is_null() {
            break;
        }
        count += 1;
    }
    unsafe { ext4rs_dir_close(iter) };
    assert!(count >= 2, "directory must still contain at least . and ..");

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}
