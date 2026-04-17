//! Extended attribute reading test against test-disks/ext4-xattr.img.
//!
//! Image was built with:
//!   /tagged.txt    user.color=red, user.com.apple.FinderInfo=<4 raw bytes>
//!   /tagged_dir    user.purpose=documents
//!   /plain.txt     (no xattrs)

use ext4rs::bgd;
use ext4rs::block_io::{BlockDevice, FileDevice};
use ext4rs::error::Result;
use ext4rs::fs::Filesystem;
use ext4rs::inode::Inode;
use ext4rs::path;
use ext4rs::xattr;
use std::sync::Arc;

const TEST_IMAGE: &str = "test-disks/ext4-xattr.img";

fn open() -> (Arc<dyn BlockDevice>, Filesystem) {
    let dev = Arc::new(FileDevice::open(TEST_IMAGE).expect("open xattr image"));
    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount");
    (dev_dyn, fs)
}

fn read_inode(fs: &Filesystem, ino: u32) -> Inode {
    let raw = read_inode_raw_bytes(fs, ino);
    Inode::parse(&raw).expect("parse")
}

fn read_inode_raw_bytes(fs: &Filesystem, ino: u32) -> Vec<u8> {
    let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino).expect("locate");
    let block_data = fs.read_block(block).expect("read block");
    let inode_size = fs.sb.inode_size as usize;
    let off = offset as usize;
    block_data[off..off + inode_size].to_vec()
}

#[test]
fn reads_user_xattrs_on_tagged_file() {
    let (dev, fs) = open();
    let mut reader = |ino: u32| -> Result<Inode> {
        let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino)?;
        let block_data = fs.read_block(block)?;
        let inode_size = fs.sb.inode_size as usize;
        let off = offset as usize;
        Inode::parse(&block_data[off..off + inode_size])
    };

    let ino = path::lookup(dev.as_ref(), &fs.sb, &mut reader, "/tagged.txt")
        .expect("resolve /tagged.txt");
    let inode = read_inode(&fs, ino);

    let entries = xattr::read_all(
        dev.as_ref(),
        &inode,
        &read_inode_raw_bytes(&fs, ino),
        fs.sb.inode_size,
        fs.sb.block_size(),
    )
    .expect("read xattrs");
    println!("xattrs on /tagged.txt:");
    for e in &entries {
        println!("  {:?} = {:?} ({} bytes)", e.name, e.value, e.value.len());
    }

    let user_xattrs: Vec<_> = entries
        .iter()
        .filter(|e| e.name.starts_with("user."))
        .collect();
    assert!(
        !user_xattrs.is_empty(),
        "expected at least one user.* xattr"
    );

    let color = entries
        .iter()
        .find(|e| e.name == "user.color")
        .expect("user.color");
    assert_eq!(color.value, b"red");

    let finder = entries
        .iter()
        .find(|e| e.name == "user.com.apple.FinderInfo")
        .expect("user.com.apple.FinderInfo");
    assert_eq!(finder.value, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn xattr_get_returns_single_value() {
    let (dev, fs) = open();
    let mut reader = |ino: u32| -> Result<Inode> {
        let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino)?;
        let block_data = fs.read_block(block)?;
        let inode_size = fs.sb.inode_size as usize;
        let off = offset as usize;
        Inode::parse(&block_data[off..off + inode_size])
    };

    let ino = path::lookup(dev.as_ref(), &fs.sb, &mut reader, "/tagged.txt")
        .expect("resolve /tagged.txt");
    let inode = read_inode(&fs, ino);

    let raw = read_inode_raw_bytes(&fs, ino);
    let v = xattr::get(
        dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        "user.color",
    )
    .expect("get user.color");
    assert_eq!(v, Some(b"red".to_vec()));

    let missing = xattr::get(
        dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        "user.does_not_exist",
    )
    .expect("get missing");
    assert_eq!(missing, None);
}

#[test]
fn directory_can_have_xattrs() {
    let (dev, fs) = open();
    let mut reader = |ino: u32| -> Result<Inode> {
        let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino)?;
        let block_data = fs.read_block(block)?;
        let inode_size = fs.sb.inode_size as usize;
        let off = offset as usize;
        Inode::parse(&block_data[off..off + inode_size])
    };

    let ino = path::lookup(dev.as_ref(), &fs.sb, &mut reader, "/tagged_dir")
        .expect("resolve /tagged_dir");
    let inode = read_inode(&fs, ino);

    let entries = xattr::read_all(
        dev.as_ref(),
        &inode,
        &read_inode_raw_bytes(&fs, ino),
        fs.sb.inode_size,
        fs.sb.block_size(),
    )
    .expect("read dir xattrs");

    let purpose = entries
        .iter()
        .find(|e| e.name == "user.purpose")
        .expect("user.purpose on tagged_dir");
    assert_eq!(purpose.value, b"documents");
}

#[test]
fn plain_file_has_no_user_xattrs() {
    let (dev, fs) = open();
    let mut reader = |ino: u32| -> Result<Inode> {
        let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino)?;
        let block_data = fs.read_block(block)?;
        let inode_size = fs.sb.inode_size as usize;
        let off = offset as usize;
        Inode::parse(&block_data[off..off + inode_size])
    };

    let ino =
        path::lookup(dev.as_ref(), &fs.sb, &mut reader, "/plain.txt").expect("resolve /plain.txt");
    let inode = read_inode(&fs, ino);

    let entries = xattr::read_all(
        dev.as_ref(),
        &inode,
        &read_inode_raw_bytes(&fs, ino),
        fs.sb.inode_size,
        fs.sb.block_size(),
    )
    .expect("read plain xattrs");

    let user_xattrs: Vec<_> = entries
        .iter()
        .filter(|e| e.name.starts_with("user."))
        .collect();
    assert!(
        user_xattrs.is_empty(),
        "/plain.txt should have no user xattrs, got: {user_xattrs:?}"
    );
}
