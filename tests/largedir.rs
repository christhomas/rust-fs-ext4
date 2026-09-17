//! LARGEDIR stress test against test-disks/ext4-largedir.img.
//!
//! Image layout (see test-disks/guest-build-images.sh build_largedir):
//!   /small.txt — "control\n"
//!   /huge/     — 70000 zero-length files named file_00001.txt .. file_70000.txt
//!                Enabled ro_compat LARGEDIR, which lifts the 2-level htree cap.
//!
//! The image is built by `chore fixtures`; these tests fail when it is absent.

use fs_ext4::bgd;
use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::dir::{self, DirEntryType};
use fs_ext4::error::Result;
use fs_ext4::file_io;
use fs_ext4::fs::Filesystem;
use fs_ext4::inode::Inode;
use fs_ext4::path;
use std::sync::Arc;

const TEST_IMAGE: &str = "ext4-largedir.img";
const EXPECTED_FILES: usize = 70000;

fn open_fixture() -> (Arc<dyn BlockDevice>, Filesystem) {
    let path = fs_ext4_test_support::fixture(env!("CARGO_MANIFEST_DIR"), TEST_IMAGE);
    let dev = Arc::new(FileDevice::open(&path).expect("open largedir image"));
    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount");
    (dev_dyn, fs)
}

fn inode_reader(fs: &Filesystem) -> impl FnMut(u32) -> Result<Inode> + '_ {
    move |ino: u32| -> Result<Inode> {
        let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino)?;
        let block_data = fs.read_block(block)?;
        let inode_size = fs.sb.inode_size as usize;
        let off = offset as usize;
        Inode::parse(&block_data[off..off + inode_size])
    }
}

fn find_huge_ino(fs: &Filesystem) -> u32 {
    let root = Inode::parse(&fs.read_inode_raw(2).unwrap()).unwrap();
    let root_data = file_io::read_all(fs, &root).unwrap();
    let block_size = fs.sb.block_size() as usize;
    let entries = dir::parse_block(&root_data[..block_size], true).unwrap();
    entries
        .iter()
        .find(|e| e.name == b"huge")
        .expect("/huge should exist in root")
        .inode
}

/// `/huge` must be flagged as an htree-indexed directory.
#[test]
fn huge_dir_is_htree_indexed() {
    let (_dev, fs) = open_fixture();

    let huge_ino = find_huge_ino(&fs);
    let huge = Inode::parse(&fs.read_inode_raw(huge_ino).unwrap()).unwrap();
    let index_flag = huge.flags & 0x1000; // EXT4_INDEX_FL
    assert_ne!(
        index_flag, 0,
        "huge_dir lacks EXT4_INDEX_FL — ext4 formatter should have htree-indexed \
         a directory with {EXPECTED_FILES} entries"
    );
    println!("/huge is htree-indexed (flags=0x{:x})", huge.flags);
    println!(
        "  size={} blocks={}",
        huge.size,
        huge.size / fs.sb.block_size() as u64
    );
}

/// LARGEDIR must be set in the superblock. It lives in the INCOMPAT mask
/// (`EXT4_FEATURE_INCOMPAT_LARGEDIR = 0x4000`), NOT RO_COMPAT — the earlier
/// version of this test had them confused.
#[test]
fn largedir_feature_is_present() {
    let (_dev, fs) = open_fixture();

    const INCOMPAT_LARGEDIR: u32 = 0x4000;
    let present = (fs.sb.feature_incompat & INCOMPAT_LARGEDIR) != 0;
    assert!(
        present,
        "superblock incompat flags=0x{:x} missing LARGEDIR (0x4000) — \
         image was built without -O large_dir",
        fs.sb.feature_incompat
    );
}

/// Path lookup through htree at three sample offsets exercises the whole tree.
#[test]
fn path_lookup_descends_htree_for_sampled_files() {
    let (dev, fs) = open_fixture();
    let dev_ref = dev.as_ref();
    let mut reader = inode_reader(&fs);

    // First, middle, last — these hash to different parts of the htree so
    // all internal nodes get exercised at least once.
    for name in ["file_00001.txt", "file_35000.txt", "file_70000.txt"] {
        let p = format!("/huge/{name}");
        let ino = path::lookup(dev_ref, &fs.sb, &mut reader, &p)
            .unwrap_or_else(|e| panic!("path::lookup failed for {p}: {e:?}"));
        assert!(ino >= 2, "suspicious inode {ino} for {p}");
        let inode = reader(ino).expect("read inode");
        // Files were created with `: >` so they're zero-length regular files.
        assert_eq!(
            inode.size, 0,
            "{name} should be zero-length, got size={}",
            inode.size
        );
        println!("{name} -> inode {ino}");
    }
}

/// A missing file inside /huge must fail NotFound, not return a random hit.
#[test]
fn missing_file_returns_not_found() {
    let (dev, fs) = open_fixture();
    let dev_ref = dev.as_ref();
    let mut reader = inode_reader(&fs);

    let result = path::lookup(dev_ref, &fs.sb, &mut reader, "/huge/not_here.txt");
    assert!(result.is_err(), "expected NotFound, got {:?}", result);
}

/// Linear scan every block of /huge, sum up the regular file entries, and
/// verify the count matches what the builder wrote. This catches htree-leaf
/// corruption or duplicate-emission bugs.
#[test]
fn linear_scan_of_all_leaf_blocks_counts_files() {
    let (_dev, fs) = open_fixture();
    let huge_ino = find_huge_ino(&fs);
    let huge = Inode::parse(&fs.read_inode_raw(huge_ino).unwrap()).unwrap();
    let huge_data = file_io::read_all(&fs, &huge).unwrap();
    let block_size = fs.sb.block_size() as usize;
    let block_count = huge_data.len() / block_size;

    let mut total_files = 0usize;
    let mut leaf_blocks = 0usize;
    for blk in 0..block_count {
        let chunk = &huge_data[blk * block_size..(blk + 1) * block_size];
        if let Ok(parsed) = dir::parse_block(chunk, true) {
            let matches = parsed
                .iter()
                .filter(|e| e.file_type == DirEntryType::RegFile && e.name.starts_with(b"file_"))
                .count();
            if matches > 0 {
                leaf_blocks += 1;
                total_files += matches;
            }
        }
        // Blocks that fail to parse linearly are dx_node / dx_root internal
        // index blocks — expected for a deep htree.
    }
    println!(
        "scanned {block_count} blocks, {leaf_blocks} linear leaves, total regfiles={total_files}"
    );
    assert_eq!(
        total_files, EXPECTED_FILES,
        "counted {total_files} file_* entries, expected {EXPECTED_FILES}"
    );
}
