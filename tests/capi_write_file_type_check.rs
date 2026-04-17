//! Type-check tests for ext4rs_write_file. Mirrors the truncate
//! guards so FSKit callers get EISDIR / EINVAL instead of Error::Corrupt
//! → EIO when the target path is the wrong file type.

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::fs;
use std::io::Write;
use std::os::raw::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const SRC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test-disks/ext4-basic.img"
);

fn scratch() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dst = PathBuf::from(format!(
        "/tmp/ext4rs_capi_write_file_type_{}_{n}.img",
        std::process::id()
    ));
    let mut out = fs::File::create(&dst).unwrap();
    out.write_all(&fs::read(SRC).unwrap()).unwrap();
    dst
}

fn last_err() -> String {
    unsafe {
        CStr::from_ptr(ext4rs_last_error()).to_string_lossy().into_owned()
    }
}

#[test]
fn write_file_on_directory_fails_with_eisdir() {
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let dir = CString::new("/subdir").unwrap();
    let payload = b"should not land anywhere";
    let rc = unsafe {
        ext4rs_write_file(
            fs_h,
            dir.as_ptr(),
            payload.as_ptr() as *const c_void,
            payload.len() as u64,
        )
    };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 21, "expected EISDIR");

    // Dir should still be enumerable.
    let iter = unsafe { ext4rs_dir_open(fs_h, dir.as_ptr()) };
    assert!(!iter.is_null());
    let mut count = 0;
    loop {
        let e = unsafe { ext4rs_dir_next(iter) };
        if e.is_null() { break; }
        count += 1;
    }
    unsafe { ext4rs_dir_close(iter) };
    assert!(count >= 2, "directory must still hold . and .. at minimum");

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn write_file_on_symlink_fails_with_einval() {
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let link = CString::new("/link.txt").unwrap();
    let payload = b"clobber";
    let rc = unsafe {
        ext4rs_write_file(
            fs_h,
            link.as_ptr(),
            payload.as_ptr() as *const c_void,
            payload.len() as u64,
        )
    };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 22, "expected EINVAL");

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn write_file_missing_path_sets_enoent() {
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let bad = CString::new("/does-not-exist.txt").unwrap();
    let payload = b"whatever";
    let rc = unsafe {
        ext4rs_write_file(
            fs_h,
            bad.as_ptr(),
            payload.as_ptr() as *const c_void,
            payload.len() as u64,
        )
    };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 2, "expected ENOENT: {}", last_err());

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn write_file_null_args_return_einval() {
    let rc = unsafe {
        ext4rs_write_file(std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), 0)
    };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 22);
}

#[test]
fn write_file_null_data_with_nonzero_len_is_einval() {
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let path = CString::new("/test.txt").unwrap();
    let rc = unsafe {
        ext4rs_write_file(fs_h, path.as_ptr(), std::ptr::null(), 4)
    };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 22);

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn write_file_zero_len_replaces_with_empty() {
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_h.is_null());

    let path = CString::new("/test.txt").unwrap();
    let rc = unsafe {
        ext4rs_write_file(fs_h, path.as_ptr(), std::ptr::null(), 0)
    };
    assert_eq!(rc, 0, "write_file with len=0 should succeed: {}", last_err());

    // File should now be empty.
    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
    unsafe { ext4rs_stat(fs_h, path.as_ptr(), &mut attr) };
    assert_eq!(attr.size, 0);

    unsafe { ext4rs_umount(fs_h) };
    let _ = fs::remove_file(&img);
}

#[test]
fn write_file_grows_content_beyond_original_size() {
    // /test.txt starts at 16 bytes. Writing a 32KB payload (>> one block)
    // must allocate new extents and produce a valid on-disk representation.
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();

    // Build a distinctive 32 KiB payload.
    let mut payload = Vec::with_capacity(32 * 1024);
    for i in 0..32 * 1024 { payload.push((i & 0xFF) as u8); }

    {
        let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
        assert!(!fs_h.is_null());
        let rc = unsafe {
            ext4rs_write_file(
                fs_h,
                path_c.as_ptr(),
                payload.as_ptr() as *const c_void,
                payload.len() as u64,
            )
        };
        assert_eq!(rc, payload.len() as i64, "write_file: {}", last_err());
        unsafe { ext4rs_umount(fs_h) };
    }

    // Remount ro — runs the full csum chain, then read back the payload.
    {
        let fs_h = unsafe { ext4rs_mount(img_c.as_ptr()) };
        assert!(!fs_h.is_null(), "remount: {}", last_err());

        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        unsafe { ext4rs_stat(fs_h, path_c.as_ptr(), &mut attr) };
        assert_eq!(attr.size, payload.len() as u64);

        let mut buf = vec![0u8; payload.len() + 16];
        let n = unsafe {
            ext4rs_read_file(
                fs_h,
                path_c.as_ptr(),
                buf.as_mut_ptr() as *mut c_void,
                0,
                buf.len() as u64,
            )
        };
        assert_eq!(n as usize, payload.len(), "read: {}", last_err());
        assert_eq!(&buf[..payload.len()], payload.as_slice(), "content mismatch");

        unsafe { ext4rs_umount(fs_h) };
    }

    let _ = fs::remove_file(&img);
}

#[test]
fn write_file_replaces_content_and_persists() {
    let img = scratch();
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();

    {
        let fs_h = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
        assert!(!fs_h.is_null());
        let payload = b"replaced\n";
        let rc = unsafe {
            ext4rs_write_file(
                fs_h,
                path_c.as_ptr(),
                payload.as_ptr() as *const c_void,
                payload.len() as u64,
            )
        };
        assert_eq!(rc, payload.len() as i64, "write: {}", last_err());
        unsafe { ext4rs_umount(fs_h) };
    }

    // Re-mount ro and read back.
    {
        let fs_h = unsafe { ext4rs_mount(img_c.as_ptr()) };
        assert!(!fs_h.is_null());
        let mut buf = [0u8; 64];
        let n = unsafe {
            ext4rs_read_file(
                fs_h,
                path_c.as_ptr(),
                buf.as_mut_ptr() as *mut c_void,
                0,
                buf.len() as u64,
            )
        };
        assert_eq!(n, 9);
        assert_eq!(&buf[..9], b"replaced\n");
        unsafe { ext4rs_umount(fs_h) };
    }

    let _ = fs::remove_file(&img);
}
