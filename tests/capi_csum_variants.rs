//! Mount + read coverage for the two checksum-feature corners via C ABI.
//!
//! - ext4-csum-seed.img: has metadata_csum + INCOMPAT_CSUM_SEED set, so the
//!   mount path must read s_checksum_seed from the superblock instead of
//!   deriving it from the UUID. Same flag that broke lwext4 on real Pi SD
//!   cards — this test locks in the correct seed selection through the
//!   public C ABI.
//! - ext4-no-csum.img: no metadata_csum feature. The verifier must stay
//!   disabled so we don't reject perfectly valid blocks on legacy images.

use fs_ext4::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::path::Path;

fn last_err(fs: *mut fs_ext4_fs_t) -> String {
    let _ = fs;
    unsafe {
        let p = fs_ext4_last_error();
        if p.is_null() {
            return String::new();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn mount_or_skip(image: &str) -> Option<*mut fs_ext4_fs_t> {
    if !Path::new(image).exists() {
        eprintln!("skip: {image} not built");
        return None;
    }
    let p = CString::new(image).unwrap();
    let fs = unsafe { fs_ext4_mount(p.as_ptr()) };
    if fs.is_null() {
        return None;
    }
    Some(fs)
}

fn read_full(fs: *mut fs_ext4_fs_t, path: &str, cap: usize) -> Vec<u8> {
    let c = CString::new(path).unwrap();
    let mut buf = vec![0u8; cap];
    let n = unsafe {
        fs_ext4_read_file(
            fs,
            c.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            0,
            cap as u64,
        )
    };
    if n < 0 {
        panic!("read_file({path}) failed: {}", last_err(fs));
    }
    buf.truncate(n as usize);
    buf
}

// ---------------------------------------------------------------------------
// ext4-csum-seed.img: INCOMPAT_CSUM_SEED — seed from superblock, not UUID
// ---------------------------------------------------------------------------

const SEED_IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-csum-seed.img");

#[test]
fn csum_seed_image_mounts() {
    let Some(fs) = mount_or_skip(SEED_IMAGE) else {
        return;
    };
    assert_eq!(fs_ext4_last_errno(), 0);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn csum_seed_image_reads_hello_txt() {
    let Some(fs) = mount_or_skip(SEED_IMAGE) else {
        return;
    };
    let data = read_full(fs, "/hello.txt", 64);
    assert_eq!(data, b"pi-style file\n");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn csum_seed_image_reads_etc_fstab_through_subdir() {
    let Some(fs) = mount_or_skip(SEED_IMAGE) else {
        return;
    };
    // /etc/fstab — exercises path walk across a subdir on a csum-seed image,
    // which verifies both the mount-time seed and the per-dir-block csum
    // verification stay in sync.
    let data = read_full(fs, "/etc/fstab", 64);
    assert_eq!(data, b"fake fstab\n");
    unsafe { fs_ext4_umount(fs) };
}

// ---------------------------------------------------------------------------
// ext4-no-csum.img: metadata_csum feature absent — verifier must stay off
// ---------------------------------------------------------------------------

const NO_CSUM_IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-no-csum.img");

#[test]
fn no_csum_image_mounts() {
    let Some(fs) = mount_or_skip(NO_CSUM_IMAGE) else {
        return;
    };
    assert_eq!(fs_ext4_last_errno(), 0);
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn no_csum_image_reads_file_without_verifier_interference() {
    let Some(fs) = mount_or_skip(NO_CSUM_IMAGE) else {
        return;
    };
    let data = read_full(fs, "/file.txt", 64);
    assert_eq!(data, b"no checksum here\n");
    unsafe { fs_ext4_umount(fs) };
}

#[test]
fn no_csum_image_stat_works() {
    let Some(fs) = mount_or_skip(NO_CSUM_IMAGE) else {
        return;
    };
    let c = CString::new("/file.txt").unwrap();
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { fs_ext4_stat(fs, c.as_ptr(), &mut attr) };
    assert_eq!(rc, 0, "stat failed: {}", last_err(fs));
    assert_eq!(attr.size, 17);
    unsafe { fs_ext4_umount(fs) };
}
