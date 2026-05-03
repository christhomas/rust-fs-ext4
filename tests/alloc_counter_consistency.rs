//! Phase 1.1 regression: every block-free path must update the bitmap,
//! the containing BGD's `bg_free_blocks_count`, AND the SB's
//! `s_free_blocks_count` together. Prior to the fix, `apply_truncate_shrink`
//! cleared the bitmap but skipped both counter writes, and `apply_unlink`
//! / `apply_replace_file_content` / `apply_rmdir` updated only one BGD
//! per op (silently miscounting fragmented files that span groups).
//!
//! These tests reproduce the simpler single-group case using
//! `ext4-basic.img`. A cross-group test would need a larger fixture; the
//! per-extent BGD update is exercised structurally — every freed run goes
//! through `free_block_run_and_bgd`, so the single-group case proves the
//! plumbing wires up correctly.

use fs_ext4::block_io::FileDevice;
use fs_ext4::path as path_mod;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, tag: &str) -> Option<String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!(
        "/tmp/fs_ext4_alloc_ctr_{}_{tag}_{n}.img",
        std::process::id()
    );
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    path_mod::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).expect("resolve")
}

fn sb_free_blocks(path: &str) -> u64 {
    let dev = FileDevice::open(path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    fs.sb.free_blocks_count
}

fn bgd_free_blocks(path: &str, gi: usize) -> u32 {
    let dev = FileDevice::open(path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    fs.groups[gi].free_blocks_count
}

#[test]
fn truncate_shrink_updates_sb_and_bgd_free_blocks() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "trunc_sb_bgd") else {
        return;
    };

    let sb_before = sb_free_blocks(&path);
    let bgd0_before = bgd_free_blocks(&path, 0);

    // Shrink /test.txt to 0 — expected to free at least one block.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("truncate");
    }

    let sb_after = sb_free_blocks(&path);
    let bgd0_after = bgd_free_blocks(&path, 0);

    assert!(
        sb_after > sb_before,
        "SB free_blocks_count did not increase: {sb_before} -> {sb_after} \
         (Phase 1.1 regression: apply_truncate_shrink skipped patch_sb_counters)"
    );
    assert!(
        bgd0_after > bgd0_before,
        "BGD[0] free_blocks_count did not increase: {bgd0_before} -> {bgd0_after} \
         (Phase 1.1 regression: apply_truncate_shrink skipped patch_bgd_counters)"
    );

    // Per-extent accounting: SB delta must equal BGD delta for a
    // single-group file (test.txt lives in group 0).
    let sb_delta = sb_after - sb_before;
    let bgd_delta = (bgd0_after - bgd0_before) as u64;
    assert_eq!(
        sb_delta, bgd_delta,
        "SB delta ({sb_delta}) != BGD delta ({bgd_delta}); counters drifted"
    );

    fs::remove_file(path).ok();
}

#[test]
fn unlink_round_trip_keeps_sb_and_bgd_in_sync() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "unlink_sb_bgd") else {
        return;
    };

    let sb_before = sb_free_blocks(&path);
    let bgd0_before = bgd_free_blocks(&path, 0);

    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_unlink("/test.txt").expect("unlink");
    }

    let sb_after = sb_free_blocks(&path);
    let bgd0_after = bgd_free_blocks(&path, 0);

    let sb_delta = sb_after - sb_before;
    let bgd_delta = (bgd0_after - bgd0_before) as u64;
    assert!(sb_delta > 0, "unlink should free at least one data block");
    assert_eq!(
        sb_delta, bgd_delta,
        "post-unlink SB ({sb_delta}) and BGD[0] ({bgd_delta}) deltas disagree"
    );

    fs::remove_file(path).ok();
}
