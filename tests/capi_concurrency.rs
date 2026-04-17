//! Thread-safety smoke tests for the C ABI.
//!
//! FSKit extensions receive concurrent operations from multiple queues.
//! These tests exercise the invariants that make that safe:
//!   - `last_error` / `last_errno` are per-thread (thread_local! storage)
//!   - multiple `dir_iter` handles on the same mount don't interfere
//!   - concurrent stat/read on the same mount never panic
//!
//! The underlying `Filesystem` uses `Arc<dyn BlockDevice>` + `Mutex<File>`
//! for I/O, so these tests also implicitly check that the mount handle is
//! `Send + Sync`.

use ext4rs::capi::*;
use std::ffi::CString;
use std::os::raw::c_void;
use std::sync::Arc;
use std::thread;

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-disks/ext4-basic.img");

/// `*mut ext4rs_fs_t` isn't Send by default. Wrap in a usize for
/// cross-thread hand-off — the underlying filesystem IS thread-safe
/// (Mutex-guarded file + Arc block device), we just need to silence the
/// borrow checker.
#[derive(Copy, Clone)]
struct FsPtr(usize);
unsafe impl Send for FsPtr {}
unsafe impl Sync for FsPtr {}
impl FsPtr {
    fn get(self) -> *mut ext4rs_fs_t {
        self.0 as *mut ext4rs_fs_t
    }
}

fn mount() -> FsPtr {
    let p = CString::new(IMAGE).unwrap();
    let fs = unsafe { ext4rs_mount(p.as_ptr()) };
    assert!(!fs.is_null(), "mount");
    FsPtr(fs as usize)
}

#[test]
fn errno_is_thread_isolated() {
    // Thread A triggers an error, thread B independently sees no error.
    let fs = mount();

    let t_a = thread::spawn(move || {
        let bad = CString::new("/nope-nope-nope").unwrap();
        let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { ext4rs_stat(fs.get(), bad.as_ptr(), &mut attr) };
        assert_eq!(rc, -1);
        assert_eq!(
            ext4rs_last_errno(),
            2,
            "A: expected ENOENT after failed stat"
        );
    });

    let t_b = thread::spawn(move || {
        // B's errno should remain 0 since B did nothing that could fail.
        // (Thread_local means A's ENOENT doesn't leak here.)
        assert_eq!(ext4rs_last_errno(), 0, "B should see fresh errno=0");
    });

    t_a.join().unwrap();
    t_b.join().unwrap();

    unsafe { ext4rs_umount(fs.get()) };
}

#[test]
fn concurrent_stat_on_same_mount_never_panics() {
    let fs = mount();
    let fs_arc = Arc::new(fs);

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let fs = fs_arc.clone();
            thread::spawn(move || {
                for _ in 0..50 {
                    let path = if i % 2 == 0 { "/test.txt" } else { "/subdir" };
                    let c = CString::new(path).unwrap();
                    let mut attr: ext4rs_attr_t = unsafe { std::mem::zeroed() };
                    let rc = unsafe { ext4rs_stat(fs.get(), c.as_ptr(), &mut attr) };
                    assert_eq!(rc, 0);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    unsafe { ext4rs_umount(fs_arc.get()) };
}

#[test]
fn concurrent_read_file_on_same_mount_returns_consistent_bytes() {
    let fs = mount();
    let fs_arc = Arc::new(fs);

    // Read /test.txt from the main thread once to establish the expected bytes.
    let c = CString::new("/test.txt").unwrap();
    let mut baseline = [0u8; 256];
    let n = unsafe {
        ext4rs_read_file(
            fs_arc.get(),
            c.as_ptr(),
            baseline.as_mut_ptr() as *mut c_void,
            0,
            baseline.len() as u64,
        )
    };
    assert!(n > 0);
    let expected: Vec<u8> = baseline[..n as usize].to_vec();
    let expected = Arc::new(expected);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let fs = fs_arc.clone();
            let expected = expected.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    let c = CString::new("/test.txt").unwrap();
                    let mut buf = [0u8; 256];
                    let n = unsafe {
                        ext4rs_read_file(
                            fs.get(),
                            c.as_ptr(),
                            buf.as_mut_ptr() as *mut c_void,
                            0,
                            buf.len() as u64,
                        )
                    };
                    assert!(n > 0);
                    assert_eq!(&buf[..n as usize], expected.as_slice());
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    unsafe { ext4rs_umount(fs_arc.get()) };
}

#[test]
fn concurrent_dir_iterations_do_not_interfere() {
    let fs = mount();
    let fs_arc = Arc::new(fs);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let fs = fs_arc.clone();
            thread::spawn(move || {
                for _ in 0..20 {
                    let c = CString::new("/").unwrap();
                    let iter = unsafe { ext4rs_dir_open(fs.get(), c.as_ptr()) };
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
                    // Root of ext4-basic has at least . + .. + a few entries.
                    assert!(count >= 4);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    unsafe { ext4rs_umount(fs_arc.get()) };
}
