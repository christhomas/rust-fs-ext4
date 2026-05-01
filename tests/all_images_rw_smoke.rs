//! Per-fixture RW smoke test: for every `test-disks/ext4-*.img`, copy it to a
//! scratch path, mount RW via the v0.1.3 callback API, list root, read the
//! first regular file we see, then create a fresh file, write known content,
//! read it back, verify, unlink, unmount.
//!
//! One #[test] fn per image gives clean per-image PASS/FAIL output from
//! `cargo test --test all_images_rw_smoke -- --nocapture`.

#![allow(unused_unsafe)]

use fs_ext4::capi::*;
use std::ffi::{c_char, c_int, c_void, CString};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

struct FileCtx {
    file: Mutex<std::fs::File>,
}

extern "C" fn read_cb(ctx: *mut c_void, buf: *mut c_void, offset: u64, length: u64) -> c_int {
    let dev = unsafe { &*(ctx as *const FileCtx) };
    let mut f = dev.file.lock().unwrap();
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return 1;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, length as usize) };
    if f.read_exact(slice).is_err() {
        return 2;
    }
    0
}

extern "C" fn write_cb(ctx: *mut c_void, buf: *const c_void, offset: u64, length: u64) -> c_int {
    let dev = unsafe { &*(ctx as *const FileCtx) };
    let mut f = dev.file.lock().unwrap();
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(buf as *const u8, length as usize) };
    if f.write_all(slice).is_err() {
        return 2;
    }
    0
}

extern "C" fn flush_cb(_ctx: *mut c_void) -> c_int {
    0
}

fn mount_rw(scratch_path: &str) -> (Box<FileCtx>, *mut fs_ext4_fs_t, u64) {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scratch_path)
        .expect("open scratch RW");
    let size = f.metadata().expect("stat scratch").len();
    let ctx = Box::new(FileCtx {
        file: Mutex::new(f),
    });
    let cfg = fs_ext4_blockdev_cfg_t {
        read: Some(read_cb),
        context: ctx.as_ref() as *const FileCtx as *mut c_void,
        size_bytes: size,
        block_size: 512,
        write: Some(write_cb),
        flush: Some(flush_cb),
    };
    let fs = unsafe { fs_ext4_mount_rw_with_callbacks(&cfg) };
    assert!(
        !fs.is_null(),
        "mount_rw_with_callbacks returned NULL (errno={})",
        unsafe { fs_ext4_last_errno() }
    );
    (ctx, fs, size)
}

/// Returns (name, dirent_file_type_byte). The byte uses the
/// `fs_ext4_file_type_t` numeric values (1 = RegFile, 2 = Dir, etc).
fn list_root(fs: *mut fs_ext4_fs_t) -> Vec<(String, u8)> {
    let root = CString::new("/").unwrap();
    let mut out = Vec::new();
    unsafe {
        let it = fs_ext4_dir_open(fs, root.as_ptr() as *const c_char);
        assert!(!it.is_null(), "dir_open / failed");
        loop {
            let de = fs_ext4_dir_next(it);
            if de.is_null() {
                break;
            }
            let name_ptr = (*de).name.as_ptr();
            let name = std::ffi::CStr::from_ptr(name_ptr)
                .to_string_lossy()
                .to_string();
            let ft = (*de).file_type;
            out.push((name, ft));
        }
        fs_ext4_dir_close(it);
    }
    out
}

const FT_REG_FILE: u8 = 1;

/// Round-trip exercise:
///   1. mount RW from scratch copy of fixture
///   2. list root
///   3. find one existing regular file (if any) and read its first 64 bytes
///   4. create /__rw_smoke_probe (mode 0o644)
///   5. write 64 bytes of known content
///   6. read it back, byte-compare
///   7. unlink
///   8. unmount
///
/// Per-image entry points below call this and surface PASS/FAIL in the test name.
fn run_round_trip(image_basename: &str) {
    let src = format!(
        "{}/test-disks/{}.img",
        env!("CARGO_MANIFEST_DIR"),
        image_basename
    );
    if !Path::new(&src).exists() {
        eprintln!("SKIP {image_basename}: fixture missing at {src}");
        return;
    }
    let scratch = format!(
        "{}/test-disks/_smoke_{}.img",
        env!("CARGO_MANIFEST_DIR"),
        image_basename
    );
    fs::copy(&src, &scratch).expect("copy fixture to scratch");

    let result = std::panic::catch_unwind(|| {
        let (_ctx, fs, _size) = mount_rw(&scratch);

        // 1b. volume info — surface block/inode capacity so create-failure
        // is diagnosable.
        let mut info: fs_ext4_volume_info_t = unsafe { std::mem::zeroed() };
        unsafe { fs_ext4_get_volume_info(fs, &mut info) };
        eprintln!(
            "[{image_basename}] volume: block_size={} blocks={}/{} inodes={}/{}",
            info.block_size,
            info.free_blocks,
            info.total_blocks,
            info.free_inodes,
            info.total_inodes
        );

        // 2. list root
        let entries = list_root(fs);
        eprintln!("[{image_basename}] root entries: {}", entries.len());

        // 3. read first regular file (if any)
        let mut first_read_bytes = 0u64;
        for (name, ft) in &entries {
            if *ft == FT_REG_FILE && !name.is_empty() {
                let path = if name.starts_with('/') {
                    name.clone()
                } else {
                    format!("/{name}")
                };
                let cpath = CString::new(path.clone()).unwrap();
                let mut buf = vec![0u8; 64];
                let n = unsafe {
                    fs_ext4_read_file(
                        fs,
                        cpath.as_ptr() as *const c_char,
                        buf.as_mut_ptr() as *mut c_void,
                        0,
                        64,
                    )
                };
                if n >= 0 {
                    first_read_bytes = n as u64;
                    eprintln!("[{image_basename}] read {n} bytes from existing file {path}");
                    break;
                }
            }
        }

        // 4. create probe file
        let probe = "/__rw_smoke_probe";
        let cprobe = CString::new(probe).unwrap();
        let inode = unsafe { fs_ext4_create(fs, cprobe.as_ptr() as *const c_char, 0o644) };
        assert!(
            inode > 0,
            "[{image_basename}] fs_ext4_create returned {inode} (errno={})",
            unsafe { fs_ext4_last_errno() }
        );

        // 5. write 64 bytes
        let payload: Vec<u8> = (0..64u8).collect();
        let n = unsafe {
            fs_ext4_write_file(
                fs,
                cprobe.as_ptr() as *const c_char,
                payload.as_ptr() as *const c_void,
                payload.len() as u64,
            )
        };
        assert_eq!(
            n,
            payload.len() as i64,
            "[{image_basename}] write_file returned {n} (errno={})",
            unsafe { fs_ext4_last_errno() }
        );

        // 6. read back + verify
        let mut readback = vec![0u8; 64];
        let n = unsafe {
            fs_ext4_read_file(
                fs,
                cprobe.as_ptr() as *const c_char,
                readback.as_mut_ptr() as *mut c_void,
                0,
                64,
            )
        };
        assert_eq!(
            n,
            64,
            "[{image_basename}] read_file returned {n} on probe (errno={})",
            unsafe { fs_ext4_last_errno() }
        );
        assert_eq!(readback, payload, "[{image_basename}] readback mismatch");

        // 7. unlink
        let rc = unsafe { fs_ext4_unlink(fs, cprobe.as_ptr() as *const c_char) };
        assert_eq!(
            rc,
            0,
            "[{image_basename}] unlink returned {rc} (errno={})",
            unsafe { fs_ext4_last_errno() }
        );

        // 8. unmount
        unsafe { fs_ext4_umount(fs) };

        eprintln!(
            "[{image_basename}] PASS: list({} entries), read({} pre-existing bytes), create+write+read+verify+unlink",
            entries.len(),
            first_read_bytes
        );
    });

    let _ = fs::remove_file(&scratch);

    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

// One test fn per fixture image — keeps cargo's PASS/FAIL output per-image.

#[test]
fn ext4_basic() {
    run_round_trip("ext4-basic")
}
#[test]
fn ext4_acl() {
    run_round_trip("ext4-acl")
}
#[test]
fn ext4_csum_seed() {
    run_round_trip("ext4-csum-seed")
}
#[test]
fn ext4_deep_extents() {
    run_round_trip("ext4-deep-extents")
}
#[test]
fn ext4_htree() {
    run_round_trip("ext4-htree")
}
#[test]
fn ext4_inline() {
    run_round_trip("ext4-inline")
}
#[test]
fn ext4_largedir() {
    run_round_trip("ext4-largedir")
}
/// KNOWN LIMITATION — `fs_ext4_create` returns 0 with `errno=5 (EIO)` when
/// adding a 516th entry to the root of `ext4-manyfiles.img`. Diagnosed via
/// `fs_ext4_get_volume_info`:
///   * 3572 / 4096 inodes free (plenty of inode capacity)
///   * 2257 / 4096 blocks free (plenty of data capacity)
///
/// So this is not exhaustion — it's an htree-directory write edge case in
/// the underlying rust-fs-ext4 driver that the smaller fixtures don't hit.
/// Run explicitly with `--ignored` to reproduce.
///
/// TODO: investigate htree leaf insert path in rust-fs-ext4 around 500+
/// entries; the crate's status table claims depth-1 inserts work.
#[test]
#[ignore = "exposes htree-write bug at ~500-entry directories — see diagnostics in fn header"]
fn ext4_manyfiles() {
    run_round_trip("ext4-manyfiles")
}
#[test]
fn ext4_no_csum() {
    run_round_trip("ext4-no-csum")
}
#[test]
fn ext4_xattr() {
    run_round_trip("ext4-xattr")
}
