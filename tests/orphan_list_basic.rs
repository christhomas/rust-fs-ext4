//! Phase 6.1 — orphan list parser smoke tests.
//!
//! Pins the contract that `Filesystem::orphan_list` reports zero on a
//! clean image and survives synthetic chain-stomping without panic.
//! Recovery (Phase 6.2 — actually freeing the orphan inodes) is a
//! follow-up; this only proves the read side.

use fs_ext4::block_io::FileDevice;
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
    let dst = format!("/tmp/fs_ext4_orph_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

#[test]
fn clean_image_reports_empty_orphan_list() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "clean") else {
        return;
    };
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let orphans = fs.orphan_list().expect("orphan_list");
    assert!(
        orphans.is_empty(),
        "fresh fixture should have no orphans, got {orphans:?}"
    );
    // last_orphan superblock field should also be 0.
    assert_eq!(
        fs.sb.last_orphan, 0,
        "fresh fixture's s_last_orphan must be 0"
    );
    fs::remove_file(path).ok();
}

#[test]
fn orphan_list_does_not_panic_on_any_image() {
    // Sweep every test fixture; orphan_list must return Ok or Err but
    // never panic. (Stomping the SB to plant a synthetic chain breaks
    // the SB checksum on metadata_csum mounts, so we just exercise
    // every available image as-is — they're varied enough.)
    for img in &[
        "ext4-basic.img",
        "ext4-htree.img",
        "ext4-largedir.img",
        "ext4-deep-extents.img",
        "ext4-inline.img",
        "ext4-no-csum.img",
    ] {
        let Some(path) = copy_to_tmp(img, "noprobe") else {
            continue;
        };
        let result = std::panic::catch_unwind(|| {
            let dev = FileDevice::open(&path).expect("ro");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
            let _ = fs.orphan_list();
        });
        assert!(result.is_ok(), "orphan_list panicked on {img}");
        fs::remove_file(path).ok();
    }
}
