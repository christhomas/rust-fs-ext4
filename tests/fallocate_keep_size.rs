//! Phase 2.2 — `fallocate(FALLOC_FL_KEEP_SIZE)` smoke + crash-safety.
//!
//! Pins:
//! - Empty file gets uninitialized extents covering the requested range.
//! - i_size unchanged; i_blocks bumps by the allocated count.
//! - Reads of the preallocated range return zeros (uninitialized extent
//!   semantics; no data on disk).
//! - SB + BGD free_blocks counters drop by exactly the allocated count.
//! - jsb.sequence advances (proves journaled multi-block transaction).
//! - Range partially covered by an existing extent is rejected with
//!   EINVAL (v1 limitation, documented in the API).

use fs_ext4::block_io::FileDevice;
use fs_ext4::file_io;
use fs_ext4::Filesystem;
use std::fs;
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
    let dst = format!("/tmp/fs_ext4_falloc_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).expect("resolve")
}

#[test]
fn fallocate_keep_size_on_empty_file_allocates_uninitialized_extent() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "empty") else {
        return;
    };

    // Truncate /test.txt to 0 first so the extent tree is empty.
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink");
    }

    let (sb_before, bgd_before, blocks_before) = {
        let dev = FileDevice::open(&path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        let (inode, _) = fs.read_inode_verified(ino).expect("read");
        (
            fs.sb.free_blocks_count,
            fs.groups[0].free_blocks_count,
            inode.blocks,
        )
    };

    // Preallocate 16 KiB (4 blocks at 4 KiB block_size). KEEP_SIZE — i_size
    // stays at 0 even though we've reserved blocks past it.
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_fallocate_keep_size(ino, 0, 16384)
            .expect("fallocate");
    }

    // Verify state.
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read");

    assert_eq!(inode.size, 0, "KEEP_SIZE should leave i_size unchanged");
    let bs = fs.sb.block_size() as u64;
    let sectors_per_block = bs / 512;
    let expected_blocks_delta = 4 * sectors_per_block;
    assert_eq!(
        inode.blocks - blocks_before,
        expected_blocks_delta,
        "i_blocks should bump by 4 fs-blocks worth of sectors"
    );
    assert_eq!(
        sb_before - fs.sb.free_blocks_count,
        4,
        "SB should record 4 blocks consumed"
    );
    assert_eq!(
        bgd_before - fs.groups[0].free_blocks_count,
        4,
        "BGD[0] should record 4 blocks consumed"
    );

    // Reads of the preallocated range must return zeros (uninitialized
    // extent semantics — no data on disk yet).
    //
    // Note: file_io::read clamps to i_size. Since KEEP_SIZE leaves i_size=0,
    // reading anything returns 0 bytes. Use the underlying extent::lookup
    // to confirm the extent IS there and uninitialized.
    let ext = fs_ext4::extent::lookup(&inode.block, fs.dev.as_ref(), fs.sb.block_size(), 0)
        .expect("lookup")
        .expect("extent present");
    assert!(
        ext.uninitialized,
        "preallocated extent should be flagged uninitialized"
    );
    assert_eq!(ext.length, 4);
    assert_eq!(ext.logical_block, 0);

    // Now grow i_size and verify file_io::read returns zeros (proves the
    // uninitialized-extent → zeros mapping in the read path).
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_truncate_grow(ino, 16384).expect("grow i_size");
    }
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let (inode, _) = fs.read_inode_verified(ino).expect("read after grow");
    let mut buf = vec![0xFFu8; 16384];
    let n = file_io::read(&fs, &inode, 0, 16384, &mut buf).expect("read");
    assert_eq!(n, 16384);
    assert!(
        buf.iter().all(|&b| b == 0),
        "preallocated range should read as zeros"
    );

    fs::remove_file(path).ok();
}

#[test]
fn fallocate_keep_size_advances_journal_sequence() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "jsb_seq") else {
        return;
    };
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink");
    }
    let seq_before = {
        let dev = FileDevice::open(&path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs_ext4::jbd2::read_superblock(&fs)
            .expect("jsb")
            .map(|j| j.sequence)
    };
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_fallocate_keep_size(ino, 0, 4096)
            .expect("fallocate");
    }
    let seq_after = {
        let dev = FileDevice::open(&path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs_ext4::jbd2::read_superblock(&fs)
            .expect("jsb")
            .map(|j| j.sequence)
    };
    if let (Some(b), Some(a)) = (seq_before, seq_after) {
        assert!(a > b, "fallocate should advance jsb.sequence ({b} -> {a})");
    }
    fs::remove_file(path).ok();
}

#[test]
fn fallocate_keep_size_rejects_partial_overlap() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "partial") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, "/test.txt");
    // /test.txt starts with non-zero size — block 0 is mapped. fallocate
    // over [0, 4096) overlaps; v1 must reject.
    let err = fs.apply_fallocate_keep_size(ino, 0, 4096).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("partially mapped"),
        "expected partial-overlap rejection: {msg}"
    );
    fs::remove_file(path).ok();
}

#[test]
fn fallocate_zero_len_is_noop() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "zero_len") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, "/test.txt");
    // len == 0 is a no-op per Linux convention.
    fs.apply_fallocate_keep_size(ino, 0, 0)
        .expect("zero-len ok");
    fs::remove_file(path).ok();
}
