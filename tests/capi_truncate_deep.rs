//! Phase 4 truncate against a file with a deep extent tree.
//!
//! ext4-deep-extents.img has /sparse.bin (16 MiB, ~245 extents, multi-level
//! extent tree). Current extent_mut.rs refuses multi-level-tree mutation
//! cleanly. This test locks in that behaviour: truncate must fail with
//! -1 and a descriptive error, never panic or corrupt the image.
//!
//! When multi-level truncate lands later, this test will start passing
//! on the success path — update the assertion accordingly.

use ext4rs::capi::*;
use std::ffi::{CStr, CString};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

const SRC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test-disks/ext4-deep-extents.img"
);

fn scratch() -> Option<PathBuf> {
    if !std::path::Path::new(SRC).exists() {
        eprintln!("skip: {SRC} not built");
        return None;
    }
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dst = PathBuf::from(format!(
        "/tmp/ext4rs_capi_truncate_deep_{}_{n}.img",
        std::process::id()
    ));
    let bytes = fs::read(SRC).expect("read src");
    let mut out = fs::File::create(&dst).expect("create");
    out.write_all(&bytes).expect("write");
    out.flush().expect("flush");
    Some(dst)
}

fn last_err() -> String {
    unsafe {
        let p = ext4rs_last_error();
        if p.is_null() {
            return String::new();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

#[test]
fn truncate_on_multi_level_extent_tree_rejects_cleanly() {
    let Some(img) = scratch() else {
        return;
    };
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let path_c = CString::new("/sparse.bin").unwrap();

    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err());

    // Try to shrink the 16 MiB sparse file to 64 KiB.
    let rc = unsafe { ext4rs_truncate(fs, path_c.as_ptr(), 65_536) };
    assert_eq!(rc, -1, "multi-level truncate must be refused");
    let err = last_err();
    assert!(
        err.contains("multi-level") || err.contains("not yet supported"),
        "expected multi-level rejection, got: {err}"
    );
    assert_ne!(ext4rs_last_errno(), 0);

    // Dense (inline-extent) file in the same image should be truncatable —
    // proves the error was specific to the deep tree, not a global failure.
    let dense = CString::new("/dense.txt").unwrap();
    let rc2 = unsafe { ext4rs_truncate(fs, dense.as_ptr(), 4) };
    assert_eq!(rc2, 0, "single-extent truncate: {}", last_err());

    unsafe { ext4rs_umount(fs) };
    let _ = fs::remove_file(&img);
}

#[test]
fn truncate_on_single_extent_file_still_works_after_deep_reject() {
    // Same idea as above but isolated, in case someone removes the dense
    // fallback from the first test: verify the straightforward case stays
    // healthy on this image.
    let Some(img) = scratch() else {
        return;
    };
    let img_c = CString::new(img.to_str().unwrap()).unwrap();
    let fs = unsafe { ext4rs_mount_rw(img_c.as_ptr()) };
    assert!(!fs.is_null(), "mount_rw: {}", last_err());
    let dense = CString::new("/dense.txt").unwrap();
    let rc = unsafe { ext4rs_truncate(fs, dense.as_ptr(), 0) };
    assert_eq!(rc, 0, "dense truncate: {}", last_err());
    unsafe { ext4rs_umount(fs) };
    let _ = fs::remove_file(&img);
}
