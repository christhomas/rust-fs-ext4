//! Regression test for the duplicate-inode-on-back-to-back-mkdir bug.
//!
//! Background: `apply_mkdir`'s allocator scans the inode bitmap to find
//! the next free inode. The scan went through `bitmap_reader` which
//! read directly from the block device. In journaled mode, the bitmap
//! write from a previous mkdir was committed to the journal log on
//! disk but the data area on disk still showed the bit as free —
//! so the next mkdir got the same inode number, producing two
//! directory entries pointing to the same on-disk inode.
//!
//! Fix: `Filesystem::mount` now wraps the device in `CachedDevice`,
//! and `commit_block_buffer` populates the cache with the post-commit
//! bytes (pinned until the next journal replay/checkpoint).
//! Subsequent reads — including the allocator's bitmap scan — see
//! the post-commit state and pick a different free inode.
//!
//! This is the bug that on macOS via FSKit caused Finder to render
//! the rename UI on `.fseventsd` whenever the user clicked
//! "New Folder". Same root cause, surfacing here as a violation of
//! "directories are uniquely identified by inode within a volume."

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str) -> Option<String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!(
        "/tmp/fs_ext4_mkdir_unique_{}_{n}_{}.img",
        std::process::id(),
        name
    );
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

#[test]
fn back_to_back_mkdir_allocates_distinct_inodes() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    // Five back-to-back mkdirs in the SAME parent directory. Each
    // call goes through the journaled bitmap-write path; without the
    // buffer-cache fix, calls 2..=5 see the pre-commit bitmap from
    // disk and re-allocate inode #1's slot.
    let mut inos = Vec::new();
    for i in 0..5u32 {
        let p = format!("/dup_dir_{i}");
        let ino = fs.apply_mkdir(&p, 0o755).expect("mkdir");
        inos.push(ino);
    }

    // No duplicates allowed — each on-disk directory must have its
    // own inode. The original bug produced [N, N, N, N, N] where N
    // was whichever inode the allocator first returned.
    let mut sorted = inos.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        inos.len(),
        "back-to-back mkdir produced duplicate inodes: {inos:?} \
         (the buffer cache isn't surfacing journaled bitmap writes \
         to the next allocator scan)"
    );

    // Sanity: each path resolves to its own inode and the inodes
    // round-trip through stat. This catches a different failure mode
    // where the allocator picks unique numbers but the bitmap write
    // never actually persists.
    for (i, &expected) in inos.iter().enumerate() {
        let p = format!("/dup_dir_{i}");
        let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
        let actual =
            fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, &p).expect("lookup");
        assert_eq!(
            actual, expected,
            "{p} resolved to a different inode than mkdir returned"
        );
    }
}

#[test]
fn back_to_back_create_allocates_distinct_inodes() {
    // Same flavour of bug but for regular file creation — the inode
    // allocator path is shared (both go through `plan_inode_allocation`),
    // so a regression here would shadow the mkdir test.
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    let mut inos = Vec::new();
    for i in 0..5u32 {
        let p = format!("/dup_file_{i}");
        let ino = fs.apply_create(&p, 0o644).expect("create");
        inos.push(ino);
    }
    let mut sorted = inos.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        inos.len(),
        "back-to-back create produced duplicate inodes: {inos:?}"
    );
}
