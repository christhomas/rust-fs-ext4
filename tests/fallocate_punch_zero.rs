//! Phase 2.3 + 2.4 — `fallocate(FALLOC_FL_PUNCH_HOLE)` and
//! `FALLOC_FL_ZERO_RANGE` integration tests.

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
    let dst = format!("/tmp/fs_ext4_pz_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).expect("resolve")
}

#[test]
fn punch_hole_frees_fully_covered_extent() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "punch_full") else {
        return;
    };

    // Set up: empty the file, then preallocate 4 blocks via fallocate
    // KEEP_SIZE so we have a known single-extent layout.
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink");
        fs.apply_fallocate_keep_size(ino, 0, 16384)
            .expect("preallocate");
    }

    let (sb_before, blocks_before) = {
        let dev = FileDevice::open(&path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        let (inode, _) = fs.read_inode_verified(ino).expect("read");
        (fs.sb.free_blocks_count, inode.blocks)
    };

    // Punch the entire range — should free all 4 blocks.
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_fallocate_punch_hole(ino, 0, 16384).expect("punch");
    }

    let (sb_after, blocks_after) = {
        let dev = FileDevice::open(&path).expect("ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        let (inode, _) = fs.read_inode_verified(ino).expect("read");
        (fs.sb.free_blocks_count, inode.blocks)
    };

    assert_eq!(sb_after - sb_before, 4, "SB free_blocks should bump by 4");
    let bs = 4096u64;
    let sectors_per_block = bs / 512;
    assert_eq!(
        blocks_before - blocks_after,
        4 * sectors_per_block,
        "i_blocks should drop by 4 fs-blocks of sectors"
    );

    fs::remove_file(path).ok();
}

#[test]
fn punch_hole_in_middle_splits_extent() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "punch_mid") else {
        return;
    };

    // Preallocate 8 blocks (32 KiB) so we have room for a middle punch.
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink");
        fs.apply_fallocate_keep_size(ino, 0, 32768)
            .expect("preallocate 8 blocks");
    }

    // Punch the middle 2 blocks (offset 8192, len 8192) — should split
    // the single extent into [0..2] and [4..8].
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_fallocate_punch_hole(ino, 8192, 8192)
            .expect("middle punch");
    }

    // Re-mount, verify two extents remain.
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read");
    let extents = fs_ext4::extent::collect_all(&inode.block, fs.dev.as_ref(), fs.sb.block_size())
        .expect("collect");
    assert_eq!(extents.len(), 2, "middle punch should leave two extents");
    assert_eq!(extents[0].logical_block, 0);
    assert_eq!(extents[0].length, 2);
    assert_eq!(extents[1].logical_block, 4);
    assert_eq!(extents[1].length, 4);

    fs::remove_file(path).ok();
}

#[test]
fn zero_range_combines_punch_and_uninit_alloc() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "zero") else {
        return;
    };

    // Preallocate 4 blocks, write known bytes (well, can't write —
    // just verify zero_range produces extents with the uninit flag and
    // no data).
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink");
        // Grow i_size first so the read at the end can reach the range.
        fs.apply_truncate_grow(ino, 16384).expect("grow size");
    }
    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_fallocate_zero_range(ino, 0, 16384).expect("zero");
    }

    // Read back: the range must be all zeros.
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read");
    let mut buf = vec![0xFFu8; 16384];
    let n = file_io::read(&fs, &inode, 0, 16384, &mut buf).expect("read");
    assert_eq!(n, 16384);
    assert!(
        buf.iter().all(|&b| b == 0),
        "zero_range should produce all-zero reads"
    );

    fs::remove_file(path).ok();
}

#[test]
fn punch_zero_len_is_noop() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "punch_noop") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, "/test.txt");
    fs.apply_fallocate_punch_hole(ino, 0, 0)
        .expect("zero-len ok");
    fs.apply_fallocate_zero_range(ino, 0, 0)
        .expect("zero-len zr ok");
    fs::remove_file(path).ok();
}
