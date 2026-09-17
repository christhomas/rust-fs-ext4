//! Integration tests for the read-only fsck-style audit.

use fs_ext4::block_io::FileDevice;
use fs_ext4::fs::Filesystem;
use fs_ext4::fsck::Anomaly;
use std::sync::Arc;

#[test]
fn pristine_basic_image_audits_clean() {
    let path = fs_ext4_test_support::fixture(env!("CARGO_MANIFEST_DIR"), "ext4-basic.img");
    let file = FileDevice::open(&path).unwrap_or_else(|e| panic!("open {path}: {e:?}"));
    let dev: Arc<dyn fs_ext4::block_io::BlockDevice> = Arc::new(file);
    let fs = Filesystem::mount(dev).expect("mount");

    let report = fs.audit(u32::MAX, u32::MAX).expect("audit");
    assert!(
        report.is_clean(),
        "pristine image should audit clean, got {:?}",
        report.anomalies
    );
    assert!(
        report.directories_scanned > 0,
        "must have scanned at least one directory"
    );
    assert!(
        report.inodes_visited >= 2,
        "must have visited root + at least one entry"
    );
}

#[test]
fn htree_image_audits_clean() {
    let path = fs_ext4_test_support::fixture(env!("CARGO_MANIFEST_DIR"), "ext4-htree.img");
    let file = FileDevice::open(&path).unwrap_or_else(|e| panic!("open {path}: {e:?}"));
    let dev: Arc<dyn fs_ext4::block_io::BlockDevice> = Arc::new(file);
    let fs = Filesystem::mount(dev).expect("mount");

    // Larger dirs — bound to 10k entries just to keep the test bounded.
    let report = fs.audit(1024, 10_000).expect("audit");
    assert!(
        report.is_clean(),
        "htree image should audit clean, got {:?}",
        report.anomalies
    );
}

#[test]
fn audit_with_zero_bounds_still_succeeds() {
    let path = fs_ext4_test_support::fixture(env!("CARGO_MANIFEST_DIR"), "ext4-basic.img");
    let file = FileDevice::open(&path).unwrap_or_else(|e| panic!("open {path}: {e:?}"));
    let dev: Arc<dyn fs_ext4::block_io::BlockDevice> = Arc::new(file);
    let fs = Filesystem::mount(dev).expect("mount");
    let report = fs.audit(0, 0).expect("audit");
    assert_eq!(report.directories_scanned, 0);
    assert_eq!(report.entries_scanned, 0);
    // Zero-bound audits are noisy (nothing scanned, everything looks
    // "too high" from the inode side) but MUST NOT panic.
    let _ = report;
}

#[test]
fn all_pristine_test_images_audit_clean() {
    let images = [
        "ext4-basic.img",
        "ext4-inline.img",
        "ext4-xattr.img",
        "ext4-acl.img",
        "ext4-deep-extents.img",
        "ext4-csum-seed.img",
        "ext4-no-csum.img",
    ];
    for name in images {
        let path = fs_ext4_test_support::fixture(env!("CARGO_MANIFEST_DIR"), name);
        let file = FileDevice::open(&path).unwrap_or_else(|e| panic!("open {path}: {e:?}"));
        let dev: Arc<dyn fs_ext4::block_io::BlockDevice> = Arc::new(file);
        let fs = Filesystem::mount(dev).expect("mount");
        let report = fs.audit(1024, 10_000).expect("audit");
        assert!(
            report.is_clean(),
            "{path} should audit clean, got {:?}",
            report.anomalies
        );
    }
}

#[test]
fn anomaly_variants_are_distinct_debug() {
    // Catch typos in Debug output that could confuse downstream consumers.
    let a = Anomaly::LinkCountTooLow {
        ino: 10,
        stored: 1,
        observed: 2,
    };
    let b = Anomaly::LinkCountTooHigh {
        ino: 10,
        stored: 3,
        observed: 2,
    };
    assert_ne!(format!("{a:?}"), format!("{b:?}"));
}
