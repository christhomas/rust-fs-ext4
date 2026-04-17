//! End-to-end test: parent dirs grow when their initial block saturates.
//!
//! Creates enough files in the root dir to force `add_entry_to_block` to
//! return `OutOfBounds`, which triggers `extend_dir_and_add_entry`. The
//! previously-failing threshold was ~100 entries on a 4 KiB-block fs.

use ext4rs::block_io::FileDevice;
use ext4rs::path as path_mod;
use ext4rs::Filesystem;
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
    let dst = format!("/tmp/ext4rs_dgrow_{}_{n}_{}.img", std::process::id(), name);
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> Option<u32> {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    path_mod::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).ok()
}

#[test]
fn apply_create_grows_parent_dir_past_first_block() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    let root_ino_before = resolve(&fs, "/").expect("root resolves");
    let (root_before, _) = fs.read_inode_verified(root_ino_before).expect("read root");
    let initial_size = root_before.size;

    // Create enough files that the first dir block overflows. 180 entries @
    // ~24 bytes each = 4320 bytes > 4 KiB usable, forcing extension.
    for i in 0..180 {
        let name = format!("/grown_{i:03}.txt");
        fs.apply_create(&name, 0o644)
            .unwrap_or_else(|e| panic!("create {name} failed at i={i}: {e}"));
    }

    // Root should now be larger (an extra block allocated via extend_dir_...).
    let (root_after, _) = fs.read_inode_verified(root_ino_before).expect("read root");
    assert!(
        root_after.size > initial_size,
        "root size should have grown past initial {} (got {})",
        initial_size,
        root_after.size,
    );

    // Spot-check a late-created file resolves and its inode is well-formed.
    let ino = resolve(&fs, "/grown_179.txt").expect("late entry resolves");
    let (inode, _) = fs.read_inode_verified(ino).expect("inode read");
    assert!(inode.is_file());

    fs::remove_file(path).ok();
}

#[test]
fn apply_mkdir_also_grows_parent() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };
    let dev = FileDevice::open_rw(&path).expect("open rw");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");

    // Fill first block past the 4 KiB boundary, then mkdir — the mkdir
    // call must trigger extend_dir_and_add_entry too. Longer names (~20 B
    // per record) make this deterministic on 4 KiB-block images.
    for i in 0..200 {
        let name = format!("/fill_file_{i:04}.log");
        fs.apply_create(&name, 0o644).expect("create");
    }
    let ino = fs
        .apply_mkdir("/new_subdir", 0o755)
        .expect("mkdir after dir-grow");
    assert!(ino > 0);
    let (dir, _) = fs.read_inode_verified(ino).expect("read new dir");
    assert!(dir.is_dir());
    assert_eq!(dir.links_count, 2);

    // Parent nlink should have bumped; size should cover ≥2 blocks.
    let root_ino = resolve(&fs, "/").unwrap();
    let (root, _) = fs.read_inode_verified(root_ino).unwrap();
    assert!(root.size >= (fs.sb.block_size() as u64) * 2);

    fs::remove_file(path).ok();
}

#[test]
fn grown_dir_survives_remount() {
    let Some(path) = copy_to_tmp("ext4-basic.img") else {
        return;
    };

    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        for i in 0..170 {
            fs.apply_create(&format!("/x_{i:03}.log"), 0o644)
                .expect("create");
        }
    }

    let dev = FileDevice::open(&path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    // All 170 entries must resolve after a cold remount.
    for i in 0..170 {
        let name = format!("/x_{i:03}.log");
        assert!(
            resolve(&fs, &name).is_some(),
            "lost entry after remount: {name}"
        );
    }

    fs::remove_file(path).ok();
}
