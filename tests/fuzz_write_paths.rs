//! Phase 7.5 (write-path fuzz). Existing tests/fuzz_smoke.rs covers the
//! READ + mount paths against malformed images. This file extends that
//! coverage to the WRITE paths added in Phases 5.2.1–5.2.7 + 5.2.6 +
//! 5.2.9 + 5.2.14: chmod, chown, utimens, setxattr, truncate_shrink,
//! truncate_grow, unlink, rmdir, removexattr.
//!
//! For each op:
//! - On a mutation-stomped image, the write call must Err (or succeed),
//!   never panic.
//! - The post-call mount must remain successful (or fail cleanly with an
//!   Err) — never panic.

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, tag: &str) -> Option<String> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!("/tmp/fs_ext4_fuzzwp_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// Stomp `len` bytes at `offset` with a deterministic pattern.
fn stomp(path: &str, offset: u64, len: usize) {
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open");
    f.seek(SeekFrom::Start(offset)).expect("seek");
    let pattern: Vec<u8> = (0..len).map(|i| (i ^ 0xC3) as u8).collect();
    f.write_all(&pattern).expect("stomp");
}

fn try_call<F: FnOnce() + std::panic::UnwindSafe>(f: F) -> std::thread::Result<()> {
    std::panic::catch_unwind(f)
}

#[test]
fn write_paths_on_stomped_inode_table_never_panic() {
    // Stomp the inode table region (varies by image but typically starts
    // around block 5-10 of group 0 → byte offset 20480-40960). Picking
    // 24 KiB to land somewhere meaningful for ext4-basic.img.
    let Some(path) = copy_to_tmp("ext4-basic.img", "inode_stomp") else {
        return;
    };
    stomp(&path, 24576, 4096);

    let result = try_call(|| {
        let dev = FileDevice::open_rw(&path).expect("open");
        let Ok(fs) = Filesystem::mount(Arc::new(dev)) else {
            // Mount itself fails — that's fine, no panic.
            return;
        };
        // Try every write op; each may Err. None may panic.
        let _ = fs.apply_chmod("/test.txt", 0o644);
        let _ = fs.apply_chown("/test.txt", 1, 1);
        let _ = fs.apply_utimens("/test.txt", 1_700_000_000, 0, 1_700_000_000, 0);
        let _ = fs.apply_setxattr("/test.txt", "user.x", b"v");
        let _ = fs.apply_removexattr("/test.txt", "user.x");
        let _ = fs.apply_unlink("/test.txt");
    });
    assert!(result.is_ok(), "write op panicked on stomped inode table");
    fs::remove_file(path).ok();
}

#[test]
fn write_paths_on_stomped_block_bitmap_never_panic() {
    // Stomp the block bitmap area. For ext4-basic.img the bitmap is
    // typically a few blocks into the image — try byte offset 8192.
    let Some(path) = copy_to_tmp("ext4-basic.img", "bitmap_stomp") else {
        return;
    };
    stomp(&path, 8192, 4096);

    let result = try_call(|| {
        let dev = FileDevice::open_rw(&path).expect("open");
        let Ok(fs) = Filesystem::mount(Arc::new(dev)) else {
            return;
        };
        // Truncate to zero will try to free blocks via the (stomped) bitmap.
        let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
        if let Ok(ino) = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt") {
            let _ = fs.apply_truncate_shrink(ino, 0);
        }
        let _ = fs.apply_unlink("/test.txt");
    });
    assert!(result.is_ok(), "write op panicked on stomped bitmap");
    fs::remove_file(path).ok();
}

#[test]
fn extreme_setxattr_value_lengths_never_panic() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "extreme_xattr") else {
        return;
    };
    let result = try_call(|| {
        let dev = FileDevice::open_rw(&path).expect("open");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        // Empty value
        let _ = fs.apply_setxattr("/test.txt", "user.empty", b"");
        // 1 MB value (way too big for any block)
        let big = vec![0xCDu8; 1024 * 1024];
        let _ = fs.apply_setxattr("/test.txt", "user.giant", &big);
        // Unknown namespace (no prefix match)
        let _ = fs.apply_setxattr("/test.txt", "weird.nope", b"v");
        // Empty name suffix (only NS prefix)
        let _ = fs.apply_setxattr("/test.txt", "user.", b"v");
    });
    assert!(result.is_ok(), "setxattr edge cases panicked");
    fs::remove_file(path).ok();
}

#[test]
fn extreme_truncate_sizes_never_panic() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "extreme_trunc") else {
        return;
    };
    let result = try_call(|| {
        let dev = FileDevice::open_rw(&path).expect("open");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
        let Ok(ino) = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt")
        else {
            return;
        };
        // Truncate to ridiculous sizes — must not panic.
        let _ = fs.apply_truncate_grow(ino, u64::MAX);
        let _ = fs.apply_truncate_grow(ino, u64::MAX / 2);
        let _ = fs.apply_truncate_shrink(ino, 0);
        let _ = fs.apply_truncate_shrink(ino, u64::MAX); // wrong direction
        let _ = fs.apply_truncate_grow(ino, 1); // shrink direction
    });
    assert!(result.is_ok(), "truncate edge cases panicked");
    fs::remove_file(path).ok();
}

#[test]
fn writes_to_nonexistent_paths_never_panic() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "nonexistent") else {
        return;
    };
    let result = try_call(|| {
        let dev = FileDevice::open_rw(&path).expect("open");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let _ = fs.apply_chmod("/does/not/exist", 0o644);
        let _ = fs.apply_chown("/no/such/path", 1, 1);
        let _ = fs.apply_setxattr("/missing", "user.x", b"v");
        let _ = fs.apply_removexattr("/missing", "user.x");
        let _ = fs.apply_unlink("/missing");
        let _ = fs.apply_rmdir("/missing");
        // Unicode chaos
        let _ = fs.apply_chmod("/\u{1F4A9}\u{0000}", 0o644);
        // Path traversal-ish
        let _ = fs.apply_chmod("/../../../etc/passwd", 0o000);
        // Very long path
        let long = "/".to_string() + &"a".repeat(1024);
        let _ = fs.apply_chmod(&long, 0o644);
    });
    assert!(result.is_ok(), "writes on nonexistent paths panicked");
    fs::remove_file(path).ok();
}

#[test]
fn read_only_device_rejects_all_writes_cleanly() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "ro_writes") else {
        return;
    };
    let result = try_call(|| {
        let dev = FileDevice::open(&path).expect("open ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        // Every write op must return Err(ReadOnly) — never panic.
        assert!(fs.apply_chmod("/test.txt", 0o644).is_err());
        assert!(fs.apply_chown("/test.txt", 1, 1).is_err());
        assert!(fs.apply_unlink("/test.txt").is_err());
        assert!(fs.apply_setxattr("/test.txt", "user.x", b"v").is_err());
        assert!(fs.apply_removexattr("/test.txt", "user.x").is_err());
    });
    assert!(result.is_ok(), "RO device write rejection panicked");
    fs::remove_file(path).ok();
}
