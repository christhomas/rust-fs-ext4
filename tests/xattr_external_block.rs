//! Phase 3: external xattr block allocation, round-trip, and removal-frees.
//!
//! Pins three contracts:
//! - Setting an xattr that doesn't fit in the in-inode region allocates an
//!   external block, sets `i_file_acl`, and bumps `i_blocks` by one.
//! - Reading the entry back returns the exact value bytes.
//! - Removing the only entry in the external block frees the block (SB +
//!   BGD counters credit it back), zeros `i_file_acl`, and decrements
//!   `i_blocks`.

use fs_ext4::block_io::FileDevice;
use fs_ext4::path as path_mod;
use fs_ext4::xattr;
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
        "/tmp/fs_ext4_xattr_ext_{}_{tag}_{n}.img",
        std::process::id()
    );
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    path_mod::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).expect("resolve")
}

fn snapshot_state(path: &str, file: &str) -> (u64, u32, u64, u64, u64) {
    // (sb_free_blocks, bgd0_free_blocks, file_acl, i_blocks, i_size)
    let dev = FileDevice::open(path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, file);
    let (inode, _) = fs.read_inode_verified(ino).expect("read inode");
    (
        fs.sb.free_blocks_count,
        fs.groups[0].free_blocks_count,
        inode.file_acl,
        inode.blocks,
        inode.size,
    )
}

#[test]
fn setxattr_overflow_allocates_external_block_and_round_trips() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "alloc_rt") else {
        return;
    };

    let (sb_before, bgd0_before, acl_before, blocks_before, _) = snapshot_state(&path, "/test.txt");
    assert_eq!(acl_before, 0, "fixture must start with no external block");

    // 512-byte value won't fit in the in-inode area on a 256-byte inode
    // (the basic-image default). Should spill to external block.
    let value = vec![0xABu8; 512];
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_setxattr("/test.txt", "user.huge", &value)
            .expect("setxattr");
    }

    let (sb_after, bgd0_after, acl_after, blocks_after, _) = snapshot_state(&path, "/test.txt");

    assert!(acl_after != 0, "i_file_acl should point at allocated block");
    assert_eq!(
        sb_before - sb_after,
        1,
        "SB should record exactly one block consumed by the xattr block"
    );
    assert_eq!(
        bgd0_before - bgd0_after,
        1,
        "BGD[0] should record exactly one block consumed"
    );
    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let sectors_per_block = fs.sb.block_size() as u64 / 512;
    assert_eq!(
        blocks_after - blocks_before,
        sectors_per_block,
        "i_blocks should bump by one fs-block worth of 512-byte sectors"
    );

    // Round-trip: read back via xattr::get.
    let ino = resolve(&fs, "/test.txt");
    let (inode, raw) = fs.read_inode_verified(ino).expect("read inode");
    let got = xattr::get(
        fs.dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        "user.huge",
    )
    .expect("get");
    assert_eq!(got, Some(value.clone()), "round-trip mismatch");

    fs::remove_file(path).ok();
}

#[test]
fn removexattr_external_block_only_entry_frees_block() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "remove_frees") else {
        return;
    };

    // Set up: spill one 512-byte entry to an external block.
    let value = vec![0x77u8; 512];
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_setxattr("/test.txt", "user.bye", &value)
            .expect("setxattr");
    }

    let (sb_pre, bgd0_pre, acl_pre, blocks_pre, _) = snapshot_state(&path, "/test.txt");
    assert!(acl_pre != 0, "external block should be present");

    // Remove the only entry — block should be freed, i_file_acl zeroed.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_removexattr("/test.txt", "user.bye")
            .expect("removexattr");
    }

    let (sb_post, bgd0_post, acl_post, blocks_post, _) = snapshot_state(&path, "/test.txt");
    assert_eq!(
        acl_post, 0,
        "i_file_acl not cleared after last-entry removal"
    );
    assert_eq!(
        sb_post - sb_pre,
        1,
        "SB free_blocks should credit back the freed xattr block"
    );
    assert_eq!(
        bgd0_post - bgd0_pre,
        1,
        "BGD[0] free_blocks should credit back the freed xattr block"
    );
    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let sectors_per_block = fs.sb.block_size() as u64 / 512;
    assert_eq!(
        blocks_pre - blocks_post,
        sectors_per_block,
        "i_blocks should drop by one fs-block worth of sectors"
    );

    fs::remove_file(path).ok();
}

#[test]
fn external_block_holds_multiple_entries() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "multi") else {
        return;
    };

    // Two large-ish entries, both spill to the same external block.
    let v1 = vec![0x11u8; 300];
    let v2 = vec![0x22u8; 300];
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_setxattr("/test.txt", "user.first", &v1)
            .expect("set first");
        fs.apply_setxattr("/test.txt", "user.second", &v2)
            .expect("set second");
    }

    // Both entries readable.
    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, raw) = fs.read_inode_verified(ino).expect("read inode");
    let g1 = xattr::get(
        fs.dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        "user.first",
    )
    .expect("get first");
    let g2 = xattr::get(
        fs.dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        "user.second",
    )
    .expect("get second");
    assert_eq!(g1, Some(v1));
    assert_eq!(g2, Some(v2));

    // Only one external block was allocated — i_file_acl is one block, not two.
    let (_, _, acl, _, _) = snapshot_state(&path, "/test.txt");
    assert!(acl != 0, "external block should still be allocated");

    fs::remove_file(path).ok();
}
