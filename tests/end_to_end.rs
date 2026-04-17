//! End-to-end: mount ext4-basic.img, list root dir, read each regular file.
//!
//! This proves the full read path works: superblock → BGD → inode → extents
//! → file_io. Uses the existing test image (no new images required).

use ext4rs::block_io::FileDevice;
use ext4rs::dir::{self, DirEntryType};
use ext4rs::file_io;
use ext4rs::fs::Filesystem;
use ext4rs::inode::Inode;
use ext4rs::superblock::SUPERBLOCK_OFFSET;
use std::sync::Arc;

const TEST_IMAGE: &str = "test-disks/ext4-basic.img";
const ROOT_INODE: u32 = 2;

#[test]
fn list_root_and_read_files() {
    let dev = Arc::new(FileDevice::open(TEST_IMAGE).expect("open image"));
    let fs = Filesystem::mount(dev).expect("mount");

    println!(
        "mounted: name={:?} block_size={} inodes={} groups={}",
        fs.sb.volume_name,
        fs.sb.block_size(),
        fs.sb.inodes_count,
        fs.sb.block_group_count()
    );

    // Read root directory inode and parse it.
    let root_raw = fs.read_inode_raw(ROOT_INODE).expect("read root inode");
    let root = Inode::parse(&root_raw).expect("parse root inode");
    assert!(root.is_dir(), "root inode 2 must be a directory");
    assert!(root.has_extents(), "root must use extents (modern ext4)");

    // Read root directory contents (block 0 of the directory file).
    let dir_data = file_io::read_all(&fs, &root).expect("read root dir data");
    assert!(!dir_data.is_empty(), "root dir must have data");

    // Parse the first block of directory entries.
    let block_size = fs.sb.block_size() as usize;
    let first_block = &dir_data[..block_size.min(dir_data.len())];
    let entries = dir::parse_block(first_block, true).expect("parse dir entries");

    println!("\nroot directory entries:");
    for e in &entries {
        let name = String::from_utf8_lossy(&e.name);
        println!("  {:>6}  type={:?}  '{}'", e.inode, e.file_type, name);
    }

    // Sanity: must contain "." and ".."
    assert!(entries.iter().any(|e| e.name == b"."), "missing .");
    assert!(entries.iter().any(|e| e.name == b".."), "missing ..");

    // For every regular-file entry, read it end-to-end.
    let mut files_read = 0;
    for e in &entries {
        if e.file_type != DirEntryType::RegFile {
            continue;
        }
        let raw = fs.read_inode_raw(e.inode).expect("read file inode");
        let inode = Inode::parse(&raw).expect("parse file inode");
        let data = file_io::read_all(&fs, &inode).expect("read file");

        let name = String::from_utf8_lossy(&e.name);
        println!(
            "\nfile '{}' (inode {}): size={} bytes, read={} bytes",
            name,
            e.inode,
            inode.size,
            data.len()
        );
        // Show up to 200 bytes of content, escape non-printable.
        let preview: String = data
            .iter()
            .take(200)
            .map(|&b| {
                if (32..127).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("  preview: {preview:?}");

        assert_eq!(
            data.len() as u64,
            inode.size,
            "read size mismatch for {name}"
        );
        files_read += 1;
    }

    println!("\nTotal regular files read: {files_read}");
    println!("Superblock offset = {SUPERBLOCK_OFFSET}");
}
