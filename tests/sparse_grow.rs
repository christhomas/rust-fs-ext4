//! Phase 2.1: sparse-file growth via truncate-up.
//!
//! `apply_truncate_grow` deliberately allocates no blocks — ext4's read
//! path returns zeros for unmapped logical blocks (sparse holes), so a
//! grow from 17 B to 1 MiB should preserve the original 17 B and read
//! zeros for everything past it. These tests pin that contract so a
//! future "emit uninitialized extents on grow" change cannot silently
//! double-allocate.
//!
//! `i_blocks` must NOT increase on a sparse grow (this is what makes
//! the file genuinely sparse — `du` reports the truth).

use fs_ext4::block_io::FileDevice;
use fs_ext4::file_io;
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
    let dst = format!("/tmp/fs_ext4_sparse_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    path_mod::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).expect("resolve")
}

#[test]
fn truncate_grow_preserves_existing_bytes() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "preserve") else {
        return;
    };

    // /test.txt is "hello from ext4.\n" (17 bytes) in the fixture.
    let original_bytes: Vec<u8> = {
        let dev = FileDevice::open(&path).expect("open ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        let (inode, _) = fs.read_inode_verified(ino).expect("read inode");
        assert!(inode.size > 0 && inode.size < 4096, "fixture sanity");
        let len = inode.size;
        let mut buf = vec![0u8; len as usize];
        file_io::read(&fs, &inode, 0, len, &mut buf).expect("read");
        buf
    };

    // Grow to 1 MiB.
    let new_size = 1024 * 1024u64;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_grow(ino, new_size).expect("grow");
    }

    // Re-read everything.
    let dev = FileDevice::open(&path).expect("open ro after");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read inode");
    assert_eq!(inode.size, new_size, "i_size did not update");

    // Original 17 bytes intact.
    let head_len = original_bytes.len() as u64;
    let mut head = vec![0xAAu8; original_bytes.len()];
    file_io::read(&fs, &inode, 0, head_len, &mut head).expect("read head");
    assert_eq!(head, original_bytes, "leading bytes corrupted by grow");

    fs::remove_file(path).ok();
}

#[test]
fn truncate_grow_hole_reads_as_zeros() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "zeros") else {
        return;
    };

    let new_size = 1024 * 1024u64;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_grow(ino, new_size).expect("grow");
    }

    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read inode");

    // Sample three offsets deep into the hole.
    for &offset in &[8192u64, 65536, new_size - 4096] {
        let mut buf = vec![0xFFu8; 4096];
        file_io::read(&fs, &inode, offset, 4096, &mut buf)
            .unwrap_or_else(|e| panic!("read @{offset}: {e}"));
        assert!(
            buf.iter().all(|&b| b == 0),
            "hole at offset {offset} not zero-filled (got non-zero byte)"
        );
    }

    fs::remove_file(path).ok();
}

#[test]
fn truncate_grow_keeps_i_blocks_constant() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "i_blocks") else {
        return;
    };

    let blocks_before = {
        let dev = FileDevice::open(&path).expect("open ro");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.read_inode_verified(ino).expect("read").0.blocks
    };

    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_grow(ino, 1024 * 1024).expect("grow");
    }

    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let blocks_after = fs.read_inode_verified(ino).expect("read").0.blocks;
    assert_eq!(
        blocks_before, blocks_after,
        "sparse grow allocated blocks (was {blocks_before}, now {blocks_after}); \
         this would break du reporting"
    );

    fs::remove_file(path).ok();
}

#[test]
fn truncate_grow_persists_across_remount() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "remount") else {
        return;
    };

    let new_size = 1024 * 1024u64;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_grow(ino, new_size).expect("grow");
    }

    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    let ino = resolve(&fs, "/test.txt");
    let (inode, _) = fs.read_inode_verified(ino).expect("read");
    assert_eq!(inode.size, new_size, "size did not survive remount");

    fs::remove_file(path).ok();
}
