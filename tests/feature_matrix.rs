//! Every feature bit the crate has an opinion about, checked against a
//! filesystem that actually carries it.
//!
//! G5 from `docs/format-conformance-gaps.md`. Every gap that document
//! records was found by *reading* the supported-feature masks and their
//! comments — not one was caught by a test. This is the test that would
//! have caught them.
//!
//! The contract per feature is deliberately weak: **read it correctly,
//! or refuse to mount.** Both are acceptable; what is not acceptable is
//! mounting and then returning data derived from arithmetic the feature
//! invalidates, which is the failure mode every gap in that document
//! shares.
//!
//! # Why this builds its own image
//!
//! `test-disks/*.img` is gitignored, and the existing builder needs an
//! Alpine VM because most fixtures require a real mount to populate.
//! The images here need only `mke2fs` — nothing is written into them,
//! the feature bits in the superblock are the whole point — so the test
//! builds them itself and runs anywhere e2fsprogs is installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::Filesystem;

/// ext4's root directory is always inode 2.
const ROOT_INO: u32 = 2;

/// Locate `mke2fs`. Homebrew keeps e2fsprogs keg-only on macOS, so it
/// is present but not on PATH.
fn mke2fs() -> Option<String> {
    if Command::new("mke2fs").arg("-V").output().is_ok() {
        return Some("mke2fs".into());
    }
    let brew = Command::new("brew")
        .args(["--prefix", "e2fsprogs"])
        .output()
        .ok()?;
    let prefix = String::from_utf8(brew.stdout).ok()?;
    let path = format!("{}/sbin/mke2fs", prefix.trim());
    Path::new(&path).exists().then_some(path)
}

/// Build a 16 MiB image with the given `mke2fs` options.
fn build(name: &str, opts: &[&str]) -> Option<PathBuf> {
    let mke2fs = mke2fs()?;
    let path =
        fs_ext4_test_support::temp_dir().join(format!("fs-ext4-fm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, vec![0u8; 16 * 1024 * 1024]).expect("allocate image");
    let out = Command::new(&mke2fs)
        .args(["-q", "-F"])
        .args(opts)
        .arg(&path)
        .output()
        .expect("run mke2fs");
    assert!(
        out.status.success(),
        "mke2fs {opts:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(path)
}

fn mount(path: &Path) -> fs_ext4::error::Result<Filesystem> {
    let dev = FileDevice::open(path.to_str().expect("utf-8 path")).expect("open image");
    let dyn_dev: Arc<dyn BlockDevice> = Arc::new(dev);
    Filesystem::mount(dyn_dev)
}

/// **G1, and what it turned out to be.** A bigalloc filesystem mounts and
/// reads its root (#75).
///
/// This was a refusal, on the premise that bigalloc moves every block-group
/// offset. It does not: `s_blocks_per_group` stays the group stride in
/// blocks, and only the bitmaps and descriptor free counts count clusters.
/// With the refusal lifted this image failed with
/// `BadChecksum { what: "block group descriptor" }`, which was taken for
/// the misreading. It was a different defect: `mke2fs -t ext4` gives a
/// 16 MiB image 1 KiB blocks, bigalloc forces `s_first_data_block = 0`,
/// and the descriptor table was located after `s_first_data_block` rather
/// than after the superblock's block. Built with `-C 16384`, sixteen blocks
/// to a cluster, so that case is the one exercised.
///
/// `tests/bigalloc_read_oracle.rs` compares file contents; this is the
/// matrix entry.
#[test]
fn bigalloc_mounts_and_reads() {
    let Some(img) = build("bigalloc", &["-t", "ext4", "-O", "bigalloc", "-C", "16384"]) else {
        eprintln!("skip: mke2fs not available (apt/brew install e2fsprogs)");
        return;
    };
    let fs = mount(&img).expect("a bigalloc filesystem reads like any other");
    assert_eq!(fs.sb.block_size(), 1024, "the case this pins is 1 KiB");
    assert_eq!(fs.sb.first_data_block, 0);
    fs.read_inode_verified(ROOT_INO)
        .expect("the root inode must be readable after mounting");
    let _ = std::fs::remove_file(&img);
}

/// The guard against fixing G1 too broadly.
///
/// It would be easy to refuse a feature by tightening the RO_COMPAT
/// check into "anything unrecognised", which would also refuse ordinary
/// filesystems carrying newer bits. This mounts a plain ext4 and
/// requires it to still work.
#[test]
fn an_ordinary_ext4_still_mounts_and_reads() {
    let Some(img) = build("plain", &["-t", "ext4"]) else {
        eprintln!("skip: mke2fs not available");
        return;
    };
    let fs = mount(&img).expect("a plain ext4 filesystem must mount");
    fs.read_inode_verified(ROOT_INO)
        .expect("the root inode must be readable after mounting");
    let _ = std::fs::remove_file(&img);
}

/// An RO_COMPAT bit the crate has never heard of must still mount —
/// that is the compatibility model, and the reason G1's refusal had to
/// be a named list rather than a mask complement.
///
/// `project` is a real RO_COMPAT feature the reader does nothing with.
#[test]
fn a_tolerated_ro_compat_feature_still_mounts() {
    let Some(img) = build("project", &["-t", "ext4", "-O", "project,quota"]) else {
        eprintln!("skip: mke2fs not available");
        return;
    };
    let fs = mount(&img).expect("project/quota are ignorable on a read-only mount");
    fs.read_inode_verified(ROOT_INO).expect("root readable");
    let _ = std::fs::remove_file(&img);
}

/// **G4.** A filesystem with Multi-Mount Protection must not be
/// mounted *writable* by a driver that does not honour it.
///
/// MMP exists to stop two hosts writing to one filesystem at the same
/// time. Honouring it means reading the MMP block, checking its
/// sequence, claiming it with our own node name and re-checking —
/// none of which this crate does. It nevertheless has twenty-one
/// `apply_*` write entry points and a live journal writer, so
/// ignoring the bit while writing is exactly the situation MMP is
/// designed to prevent.
///
/// Read-only is still allowed and is deliberately checked below: a
/// user recovering data from a disk another machine has open is the
/// case that must keep working.
#[test]
fn an_mmp_filesystem_is_refused_for_writing_but_allowed_read_only() {
    let Some(img) = build("mmp", &["-t", "ext4", "-O", "mmp"]) else {
        eprintln!("skip: mke2fs not available");
        return;
    };

    // Writable: must be refused.
    let dev = FileDevice::open_rw(img.to_str().unwrap()).expect("open read-write");
    let writable: Arc<dyn BlockDevice> = Arc::new(dev);
    assert!(
        writable.is_writable(),
        "the fixture must be writable, or this test proves nothing"
    );
    assert!(
        Filesystem::mount(writable).is_err(),
        "an MMP filesystem MOUNTED WRITABLE. MMP exists to stop two hosts \
         writing at once, and this crate does not honour it — mounting \
         writable means becoming the second writer MMP was protecting \
         against."
    );

    // Read-only: must still work.
    let ro = FileDevice::open(img.to_str().unwrap()).expect("open read-only");
    let ro_dyn: Arc<dyn BlockDevice> = Arc::new(ro);
    Filesystem::mount(ro_dyn).expect("a read-only mount of an MMP filesystem must still work");

    let _ = std::fs::remove_file(&img);
}
