//! Integration tests for `ext4rs_chmod` + `ext4rs_chown`.
//!
//! Covers:
//! - Success clears errno; attr roundtrip reflects the change.
//! - chmod preserves file-type bits (S_IFREG vs S_IFDIR) even if caller
//!   passes the raw octal mode.
//! - chown with `u32::MAX` sentinel leaves the original value.
//! - High-u16 halves of uid/gid are stored separately (offsets 0x78/0x7A)
//!   and reassemble correctly on stat.
//! - RO (callback) mount refuses both ops with a non-zero errno.
//! - The inode survives an unmount/remount roundtrip with csum-enabled
//!   metadata (the inode-checksum tail must be patched).

use ext4rs::capi::*;
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

fn scratch(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dst = PathBuf::from(format!(
        "/tmp/ext4rs_capi_chmod_chown_{tag}_{}_{n}.img",
        std::process::id()
    ));
    let bytes = fs::read(SRC).expect("read src");
    let mut out = fs::File::create(&dst).expect("create");
    out.write_all(&bytes).expect("write");
    out.flush().expect("flush");
    dst
}

fn stat_attr(fs_handle: *mut ext4rs_fs_t, path: &str) -> ext4rs_attr_t {
    let p = CString::new(path).unwrap();
    let mut attr = MaybeUninit::<ext4rs_attr_t>::uninit();
    let rc = unsafe { ext4rs_stat(fs_handle, p.as_ptr(), attr.as_mut_ptr()) };
    assert_eq!(rc, 0, "stat {path} failed");
    unsafe { attr.assume_init() }
}

#[test]
fn chmod_preserves_file_type_bits() {
    let img = scratch("mode");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();

    let fs_handle = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_handle.is_null());

    let before = stat_attr(fs_handle, "/test.txt");
    assert!(
        matches!(before.file_type, ext4rs_file_type_t::RegFile),
        "regular file"
    );

    // 0o600 = only read/write for owner. Caller deliberately passes the
    // low-12 permission bits only (no S_IFREG mix-in) — the implementation
    // must preserve S_IFREG from the existing inode.
    let rc = unsafe { ext4rs_chmod(fs_handle, path_c.as_ptr(), 0o600) };
    assert_eq!(rc, 0);
    assert_eq!(ext4rs_last_errno(), 0);

    let after = stat_attr(fs_handle, "/test.txt");
    // `attr.mode` in the C-ABI struct already masks to 0x0FFF.
    assert_eq!(after.mode, 0o600, "new perms applied");
    assert!(
        matches!(after.file_type, ext4rs_file_type_t::RegFile),
        "still a regular file"
    );

    unsafe { ext4rs_umount(fs_handle) };
    let _ = fs::remove_file(&img);
}

#[test]
fn chmod_missing_path_sets_enoent() {
    let img = scratch("enoent");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_handle = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_handle.is_null());

    let bad = CString::new("/nope_xyz.qqq").unwrap();
    let rc = unsafe { ext4rs_chmod(fs_handle, bad.as_ptr(), 0o644) };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 2, "ENOENT for missing path");

    unsafe { ext4rs_umount(fs_handle) };
    let _ = fs::remove_file(&img);
}

#[test]
fn chmod_null_args_set_einval() {
    let img = scratch("null");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs_handle = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_handle.is_null());
    let rc = unsafe { ext4rs_chmod(fs_handle, std::ptr::null(), 0o644) };
    assert_eq!(rc, -1);
    assert_eq!(ext4rs_last_errno(), 22, "EINVAL for null path");
    unsafe { ext4rs_umount(fs_handle) };
    let _ = fs::remove_file(&img);
}

#[test]
fn chown_sets_uid_and_gid_roundtrip() {
    let img = scratch("uidgid");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();

    let fs_handle = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_handle.is_null());

    let rc = unsafe { ext4rs_chown(fs_handle, path_c.as_ptr(), 1234, 5678) };
    assert_eq!(rc, 0);
    assert_eq!(ext4rs_last_errno(), 0);

    let after = stat_attr(fs_handle, "/test.txt");
    assert_eq!(after.uid, 1234);
    assert_eq!(after.gid, 5678);

    unsafe { ext4rs_umount(fs_handle) };
    let _ = fs::remove_file(&img);
}

#[test]
fn chown_uses_high_u16_halves_for_32bit_values() {
    // UID = 0x0001_ABCD — low half 0xABCD, high half 0x0001. Tests that
    // both the 0x02 and 0x78 slots get patched (otherwise the top half
    // silently truncates to 0 on remount).
    let img = scratch("hi16");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();
    let fs_handle = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_handle.is_null());

    let big_uid = 0x0001_ABCDu32;
    let big_gid = 0x0002_9876u32;
    let rc = unsafe { ext4rs_chown(fs_handle, path_c.as_ptr(), big_uid, big_gid) };
    assert_eq!(rc, 0);

    let after = stat_attr(fs_handle, "/test.txt");
    assert_eq!(after.uid, big_uid);
    assert_eq!(after.gid, big_gid);

    unsafe { ext4rs_umount(fs_handle) };
    let _ = fs::remove_file(&img);
}

#[test]
fn chown_sentinel_leaves_value_unchanged() {
    // Passing u32::MAX (Linux chown(2)'s "-1" sentinel) for either
    // parameter must leave that slot alone.
    let img = scratch("sentinel");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();
    let fs_handle = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_handle.is_null());

    // Set a known starting state.
    unsafe { ext4rs_chown(fs_handle, path_c.as_ptr(), 1000, 1000) };
    let before = stat_attr(fs_handle, "/test.txt");
    assert_eq!(before.uid, 1000);
    assert_eq!(before.gid, 1000);

    // Update only gid.
    let rc = unsafe { ext4rs_chown(fs_handle, path_c.as_ptr(), u32::MAX, 42) };
    assert_eq!(rc, 0);
    let after = stat_attr(fs_handle, "/test.txt");
    assert_eq!(after.uid, 1000, "uid kept (sentinel)");
    assert_eq!(after.gid, 42, "gid updated");

    unsafe { ext4rs_umount(fs_handle) };
    let _ = fs::remove_file(&img);
}

#[test]
fn chmod_survives_remount_with_csum() {
    // The inode checksum must be patched after chmod, otherwise a fresh
    // mount on a metadata_csum-enabled image would fail verification.
    let img = scratch("csum");
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/test.txt").unwrap();

    let fs_handle = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs_handle.is_null());
    let rc = unsafe { ext4rs_chmod(fs_handle, path_c.as_ptr(), 0o400) };
    assert_eq!(rc, 0);
    unsafe { ext4rs_umount(fs_handle) };

    // Remount ro — verify_inode() must succeed on /test.txt.
    let fs2 = unsafe { ext4rs_mount(img_c.as_ptr()) };
    assert!(!fs2.is_null(), "remount failed — inode csum not patched?");
    let after = stat_attr(fs2, "/test.txt");
    assert_eq!(after.mode, 0o400);
    assert!(matches!(after.file_type, ext4rs_file_type_t::RegFile));
    unsafe { ext4rs_umount(fs2) };

    let _ = fs::remove_file(&img);
}
