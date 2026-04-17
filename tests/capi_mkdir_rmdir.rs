//! C-ABI tests for `ext4rs_mkdir` and `ext4rs_rmdir`. Covers the
//! full Finder "new folder" / "move to trash (for an empty folder)" round
//! trip, plus edge cases (existing target, non-empty rmdir, regular-file
//! rmdir refusal).

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const SRC_IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

fn last_err_str() -> String {
    unsafe {
        let p = ext4rs_last_error();
        if p.is_null() {
            return "<null>".into();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn scratch_image() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dst = PathBuf::from(format!(
        "/tmp/ext4rs_capi_mkdir_rmdir_{}_{n}.img",
        std::process::id()
    ));
    let bytes = std::fs::read(SRC_IMAGE).expect("read src image");
    let mut out = std::fs::File::create(&dst).expect("create dst image");
    out.write_all(&bytes).expect("write dst image");
    out.flush().expect("flush");
    drop(out);
    dst
}

fn path_exists(fs: *mut ext4rs_fs_t, path: &str) -> bool {
    let p = CString::new(path).unwrap();
    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    unsafe { ext4rs_stat(fs, p.as_ptr(), &mut attr as *mut _) == 0 }
}

fn stat(fs: *mut ext4rs_fs_t, path: &str) -> ext4rs_attr_t {
    let p = CString::new(path).unwrap();
    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { ext4rs_stat(fs, p.as_ptr(), &mut attr as *mut _) };
    assert_eq!(rc, 0, "stat {path}: {}", last_err_str());
    attr
}

#[test]
fn mkdir_creates_visible_directory_and_persists() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path = "/fresh";
    let path_c = CString::new(path).unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());

    let ino = unsafe { ext4rs_mkdir(fs, path_c.as_ptr(), 0o755) };
    assert!(ino > 0, "mkdir: {}", last_err_str());
    assert!(path_exists(fs, path));
    let a = stat(fs, path);
    assert_eq!(a.inode, ino);
    // file_type reported as Dir
    assert_eq!(a.file_type as u8, ext4rs_file_type_t::Dir as u8);
    // perm bits preserved
    assert_eq!(a.mode, 0o755);

    unsafe { ext4rs_umount(fs) };

    // Persists across remount.
    let fs2 = unsafe { ext4rs_mount(img_c.as_ptr()) };
    assert!(!fs2.is_null(), "remount: {}", last_err_str());
    assert!(path_exists(fs2, path));
    let a2 = stat(fs2, path);
    assert_eq!(a2.inode, ino);
    assert_eq!(a2.file_type as u8, ext4rs_file_type_t::Dir as u8);
    unsafe { ext4rs_umount(fs2) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn mkdir_then_create_file_inside_round_trip() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let dir_c = CString::new("/newdir").unwrap();
    let file_c = CString::new("/newdir/inside.txt").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());

    let dir_ino = unsafe { ext4rs_mkdir(fs, dir_c.as_ptr(), 0o755) };
    assert!(dir_ino > 0, "mkdir: {}", last_err_str());
    let file_ino = unsafe { ext4rs_create(fs, file_c.as_ptr(), 0o644) };
    assert!(file_ino > 0, "create in new dir: {}", last_err_str());
    assert!(path_exists(fs, "/newdir/inside.txt"));
    unsafe { ext4rs_umount(fs) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn mkdir_refuses_existing_path() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    // /subdir already exists on ext4-basic.img.
    let path_c = CString::new("/subdir").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());
    let ino = unsafe { ext4rs_mkdir(fs, path_c.as_ptr(), 0o755) };
    assert_eq!(ino, 0, "mkdir duplicate must fail");
    let err = last_err_str();
    assert!(err.contains("exist"), "expected exists in error: {err}");
    unsafe { ext4rs_umount(fs) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn mkdir_refuses_missing_parent() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/nope/child").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());
    let ino = unsafe { ext4rs_mkdir(fs, path_c.as_ptr(), 0o755) };
    assert_eq!(ino, 0);
    unsafe { ext4rs_umount(fs) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn mkdir_refuses_on_ro_mount() {
    let img_c = CString::new(SRC_IMAGE).unwrap();
    let path_c = CString::new("/wont_appear").unwrap();

    let fs = unsafe { ext4rs_mount(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount: {}", last_err_str());
    let ino = unsafe { ext4rs_mkdir(fs, path_c.as_ptr(), 0o755) };
    assert_eq!(ino, 0);
    let err = last_err_str();
    assert!(err.contains("read-only") || err.contains("apply_mkdir"), "RO error: {err}");
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn rmdir_removes_empty_directory_and_persists() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/tmpdir").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());

    let ino = unsafe { ext4rs_mkdir(fs, path_c.as_ptr(), 0o755) };
    assert!(ino > 0, "mkdir: {}", last_err_str());

    let rc = unsafe { ext4rs_rmdir(fs, path_c.as_ptr()) };
    assert_eq!(rc, 0, "rmdir: {}", last_err_str());
    assert!(!path_exists(fs, "/tmpdir"));

    unsafe { ext4rs_umount(fs) };

    let fs2 = unsafe { ext4rs_mount(img_c.as_ptr()) };
    assert!(!fs2.is_null(), "remount: {}", last_err_str());
    assert!(!path_exists(fs2, "/tmpdir"), "dir should stay gone after remount");
    unsafe { ext4rs_umount(fs2) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn rmdir_refuses_non_empty_directory() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let dir_c = CString::new("/nested").unwrap();
    let file_c = CString::new("/nested/child.txt").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());
    let d = unsafe { ext4rs_mkdir(fs, dir_c.as_ptr(), 0o755) };
    assert!(d > 0, "mkdir: {}", last_err_str());
    let f = unsafe { ext4rs_create(fs, file_c.as_ptr(), 0o644) };
    assert!(f > 0, "create child: {}", last_err_str());

    let rc = unsafe { ext4rs_rmdir(fs, dir_c.as_ptr()) };
    assert_eq!(rc, -1, "rmdir on non-empty must fail");
    let err = last_err_str();
    assert!(
        err.contains("not empty") || err.contains("ENOTEMPTY"),
        "error should mention non-empty: {err}"
    );
    // Clean up the child first, THEN rmdir should succeed.
    let rc = unsafe { ext4rs_unlink(fs, file_c.as_ptr()) };
    assert_eq!(rc, 0, "unlink child: {}", last_err_str());
    let rc = unsafe { ext4rs_rmdir(fs, dir_c.as_ptr()) };
    assert_eq!(rc, 0, "rmdir after emptying: {}", last_err_str());
    unsafe { ext4rs_umount(fs) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn rmdir_refuses_regular_file() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());
    let rc = unsafe { ext4rs_rmdir(fs, path_c.as_ptr()) };
    assert_eq!(rc, -1);
    assert!(path_exists(fs, "/test.txt"));
    unsafe { ext4rs_umount(fs) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn rmdir_refuses_missing_path() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/nope").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());
    let rc = unsafe { ext4rs_rmdir(fs, path_c.as_ptr()) };
    assert_eq!(rc, -1);
    unsafe { ext4rs_umount(fs) };

    std::fs::remove_file(&img).ok();
}

#[test]
fn rmdir_refuses_on_ro_mount() {
    let img_c = CString::new(SRC_IMAGE).unwrap();
    let path_c = CString::new("/subdir").unwrap();

    let fs = unsafe { ext4rs_mount(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount: {}", last_err_str());
    let rc = unsafe { ext4rs_rmdir(fs, path_c.as_ptr()) };
    assert_eq!(rc, -1);
    unsafe { ext4rs_umount(fs) };
}

#[test]
fn mkdir_rmdir_null_inputs_do_not_crash() {
    let img = scratch_image();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err_str());

    let p = CString::new("/x").unwrap();
    assert_eq!(unsafe { ext4rs_mkdir(std::ptr::null_mut(), p.as_ptr(), 0o755) }, 0);
    assert_eq!(unsafe { ext4rs_mkdir(fs, std::ptr::null(), 0o755) }, 0);
    assert_eq!(unsafe { ext4rs_rmdir(std::ptr::null_mut(), p.as_ptr()) }, -1);
    assert_eq!(unsafe { ext4rs_rmdir(fs, std::ptr::null()) }, -1);

    unsafe { ext4rs_umount(fs) };
    std::fs::remove_file(&img).ok();
}
