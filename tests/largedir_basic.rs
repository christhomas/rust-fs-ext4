//! LARGEDIR stress test — 70,000 entries in one directory on a filesystem
//! mounted with `INCOMPAT_LARGEDIR`.
//!
//! Image (see build-ext4-feature-images.sh build_largedir):
//!   /huge/       70000 zero-length files file_00001.txt .. file_70000.txt
//!   /small.txt   control file
//!
//! 70k entries comfortably exceeds the legacy 2-level htree cap, so these
//! tests exercise `htree::lookup_leaf` descending through multiple internal
//! levels — the code path LARGEDIR enables.

use ext4rs::bgd;
use ext4rs::block_io::{BlockDevice, FileDevice};
use ext4rs::error::Result;
use ext4rs::fs::Filesystem;
use ext4rs::inode::Inode;
use ext4rs::path;
use std::path::Path;
use std::sync::Arc;

const TEST_IMAGE: &str = "test-disks/ext4-largedir.img";

fn open_or_skip() -> Option<(Arc<dyn BlockDevice>, Filesystem)> {
    if !Path::new(TEST_IMAGE).exists() {
        eprintln!("skip: {TEST_IMAGE} not built; run build-ext4-feature-images.sh largedir");
        return None;
    }
    let dev = Arc::new(FileDevice::open(TEST_IMAGE).expect("open largedir image"));
    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount");
    Some((dev_dyn, fs))
}

fn inode_reader(
    fs: &Filesystem,
) -> impl FnMut(u32) -> Result<Inode> + '_ {
    move |ino: u32| -> Result<Inode> {
        let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino)?;
        let block_data = fs.read_block(block)?;
        let inode_size = fs.sb.inode_size as usize;
        let off = offset as usize;
        Inode::parse(&block_data[off..off + inode_size])
    }
}

fn resolve(dev: &dyn BlockDevice, fs: &Filesystem, p: &str) -> u32 {
    let mut reader = inode_reader(fs);
    path::lookup(dev, &fs.sb, &mut reader, p).unwrap_or_else(|e| panic!("resolve {p}: {e}"))
}

#[test]
fn largedir_mount_succeeds_and_sees_control_file() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    // mount already succeeded; sanity check the control file via linear path.
    let ino = resolve(dev.as_ref(), &fs, "/small.txt");
    assert!(ino >= 2);
}

#[test]
fn htree_resolves_boundary_entries() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    // First, last, and a couple of interior names — if any of these hits a
    // wrong tree leaf the resolve will fail or return the wrong inode.
    for name in ["file_00001.txt", "file_00002.txt", "file_35000.txt", "file_69999.txt", "file_70000.txt"] {
        let p = format!("/huge/{name}");
        let ino = resolve(dev.as_ref(), &fs, &p);
        assert!(ino >= 2, "resolve {p} -> {ino}");
    }
}

#[test]
fn htree_random_sample_all_resolve() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    // Deterministic "random" sample — evenly spaced across the 70k range.
    for i in (1..=70_000u32).step_by(517) {
        let p = format!("/huge/file_{i:05}.txt");
        let ino = resolve(dev.as_ref(), &fs, &p);
        assert!(ino >= 2, "resolve {p} -> {ino}");
    }
}

#[test]
fn missing_entry_returns_notfound() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    let mut reader = inode_reader(&fs);
    // Name that clearly isn't in the 1..=70000 range.
    let err = path::lookup(
        dev.as_ref(),
        &fs.sb,
        &mut reader,
        "/huge/file_99999.txt",
    )
    .unwrap_err();
    use ext4rs::error::Error;
    assert!(matches!(err, Error::NotFound));
}
