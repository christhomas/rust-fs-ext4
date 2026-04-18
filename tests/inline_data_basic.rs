//! Inline data integration test against test-disks/ext4-inline.img.

use ext4rs::bgd;
use ext4rs::block_io::{BlockDevice, FileDevice};
use ext4rs::error::Result;
use ext4rs::file_io;
use ext4rs::fs::Filesystem;
use ext4rs::inode::{Inode, InodeFlags};
use ext4rs::path;
use std::sync::Arc;

const TEST_IMAGE: &str = "test-disks/ext4-inline.img";

fn open() -> (Arc<dyn BlockDevice>, Filesystem) {
    let dev = Arc::new(FileDevice::open(TEST_IMAGE).expect("open inline image"));
    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount");
    (dev_dyn, fs)
}

fn read_inode_raw_bytes(fs: &Filesystem, ino: u32) -> Vec<u8> {
    let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino).expect("locate");
    let block_data = fs.read_block(block).expect("read block");
    let inode_size = fs.sb.inode_size as usize;
    let off = offset as usize;
    block_data[off..off + inode_size].to_vec()
}

fn lookup_and_read(path_str: &str) -> (Inode, Vec<u8>) {
    let (dev, fs) = open();
    let mut reader = |ino: u32| -> Result<Inode> {
        let raw = read_inode_raw_bytes(&fs, ino);
        Inode::parse(&raw)
    };

    let ino = path::lookup(dev.as_ref(), &fs.sb, &mut reader, path_str)
        .unwrap_or_else(|e| panic!("lookup {path_str}: {e}"));
    let inode = reader(ino).expect("inode");
    let raw = read_inode_raw_bytes(&fs, ino);

    let data = if (inode.flags & InodeFlags::INLINE_DATA.bits()) != 0 {
        file_io::read_inline(&fs, &inode, &raw).expect("read_inline")
    } else {
        file_io::read_all(&fs, &inode).expect("read_all")
    };
    (inode, data)
}

#[test]
fn tiny_inline_file_round_trips() {
    let (inode, data) = lookup_and_read("/tiny.txt");
    println!(
        "tiny.txt: size={} flags=0x{:x} inline={}",
        inode.size,
        inode.flags,
        (inode.flags & InodeFlags::INLINE_DATA.bits()) != 0
    );
    println!("content: {:?}", String::from_utf8_lossy(&data));
    assert_eq!(inode.size, 12, "size = 'tiny inline\\n'");
    assert_eq!(data, b"tiny inline\n");
    assert!(
        (inode.flags & InodeFlags::INLINE_DATA.bits()) != 0,
        "tiny.txt should have INLINE_DATA flag"
    );
}

#[test]
fn medium_inline_file_uses_xattr_overflow() {
    let (inode, data) = lookup_and_read("/medium.txt");
    println!(
        "medium.txt: size={} flags=0x{:x} bytes_in_block={}",
        inode.size,
        inode.flags,
        data.len().min(60)
    );
    let expected: Vec<u8> = std::iter::repeat_n(b'A', 100).collect();
    assert_eq!(inode.size, 100);
    assert_eq!(
        data,
        expected,
        "medium.txt content (got {} bytes, want 100)",
        data.len()
    );
    assert!((inode.flags & InodeFlags::INLINE_DATA.bits()) != 0);
}

#[test]
fn read_with_raw_dispatches_correctly() {
    let (dev, fs) = open();
    let mut reader = |ino: u32| -> Result<Inode> {
        let raw = read_inode_raw_bytes(&fs, ino);
        Inode::parse(&raw)
    };
    let ino = path::lookup(dev.as_ref(), &fs.sb, &mut reader, "/medium.txt").unwrap();
    let inode = reader(ino).unwrap();
    let raw = read_inode_raw_bytes(&fs, ino);

    let mut buf = vec![0u8; 100];
    let n = file_io::read_with_raw(&fs, &inode, &raw, 0, 100, &mut buf).unwrap();
    assert_eq!(n, 100);
    assert!(buf.iter().all(|&b| b == b'A'));

    // Partial read at offset 50
    let mut buf2 = vec![0u8; 30];
    let n2 = file_io::read_with_raw(&fs, &inode, &raw, 50, 30, &mut buf2).unwrap();
    assert_eq!(n2, 30);
    assert!(buf2.iter().all(|&b| b == b'A'));
}
