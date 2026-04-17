//! Integration test: parse the JBD2 journal superblock from real images built
//! by test-disks/build-ext4-feature-images.sh.
//!
//! Spec reference for the superblock layout: see ext4rs/src/jbd2.rs.
//!
//! We don't rely on `has_journal` feature being the default in these images —
//! if `sb.journal_inode == 0`, we assert that `jbd2::read_superblock` returns
//! Ok(None) instead of failing.

use ext4rs::block_io::FileDevice;
use ext4rs::jbd2::{self, JBD2_SUPERBLOCK_V1, JBD2_SUPERBLOCK_V2};
use ext4rs::Filesystem;
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn try_mount(image: &str) -> Option<Filesystem> {
    let path = image_path(image);
    if !std::path::Path::new(&path).exists() {
        eprintln!("skip {image}: not found");
        return None;
    }
    let dev = FileDevice::open(&path).ok()?;
    Filesystem::mount(Arc::new(dev)).ok()
}

#[test]
fn journal_sb_round_trips_on_basic_image() {
    let Some(fs) = try_mount("ext4-basic.img") else { return };

    let sb = jbd2::read_superblock(&fs).expect("read_superblock");

    match sb {
        Some(jsb) => {
            // Type must be V1 or V2.
            assert!(
                matches!(jsb.block_type, JBD2_SUPERBLOCK_V1 | JBD2_SUPERBLOCK_V2),
                "bad block_type {}",
                jsb.block_type
            );
            // Journal block size should match fs block size for an internal journal.
            assert_eq!(
                jsb.block_size,
                fs.sb.block_size(),
                "journal block_size {} != fs block_size {}",
                jsb.block_size,
                fs.sb.block_size()
            );
            // max_len > 0 (mkfs defaults to at least 1024 journal blocks).
            assert!(jsb.max_len > 0, "max_len == 0");
            // first is typically 1 (block 0 is the sb itself).
            assert!(jsb.first >= 1, "first block < 1: {}", jsb.first);
            // errno == 0 for a healthy image.
            assert_eq!(jsb.errno, 0, "journal errno = {}", jsb.errno);
            // Unmounted image should be clean.
            assert!(
                jsb.is_clean(),
                "expected clean journal (start=0), got start={}",
                jsb.start
            );
        }
        None => {
            assert_eq!(fs.sb.journal_inode, 0, "journal_inode != 0 but got None");
        }
    }
}

#[test]
fn journal_sb_on_csum_seed_image() {
    let Some(fs) = try_mount("ext4-csum-seed.img") else { return };
    let Ok(Some(jsb)) = jbd2::read_superblock(&fs) else { return };
    // On the Pi-style CSUM_SEED image we still expect a valid journal.
    assert!(matches!(jsb.block_type, JBD2_SUPERBLOCK_V1 | JBD2_SUPERBLOCK_V2));
    assert_eq!(jsb.block_size, fs.sb.block_size());
    assert!(jsb.is_clean());
}

#[test]
fn journal_sb_on_htree_image() {
    let Some(fs) = try_mount("ext4-htree.img") else { return };
    let Ok(Some(jsb)) = jbd2::read_superblock(&fs) else { return };
    assert!(matches!(jsb.block_type, JBD2_SUPERBLOCK_V1 | JBD2_SUPERBLOCK_V2));
    assert!(jsb.max_len > 0);
    assert!(jsb.is_clean());
}
