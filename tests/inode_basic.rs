//! Read inode #2 (root dir) from the basic test image and validate parsing.

use std::sync::Arc;

use fs_ext4::block_io::FileDevice;
use fs_ext4::inode::Inode;
use fs_ext4::Filesystem;

const TEST_IMAGE: &str = "test-disks/ext4-basic.img";

#[test]
fn root_inode_parses() {
    let dev = Arc::new(FileDevice::open(TEST_IMAGE).expect("open test image"));
    let fs = Filesystem::mount(dev).expect("mount fs");

    let raw = fs.read_inode_raw(2).expect("read inode 2");
    assert_eq!(raw.len(), fs.sb.inode_size as usize);

    let ino = Inode::parse(&raw).expect("parse inode 2");

    println!("root inode:");
    println!("  mode        = 0o{:o}", ino.mode);
    println!("  uid/gid     = {}/{}", ino.uid, ino.gid);
    println!("  size        = {}", ino.size);
    println!("  links_count = {}", ino.links_count);
    println!("  blocks(512) = {}", ino.blocks);
    println!("  flags       = 0x{:08x} ({:?})", ino.flags, ino.flag_set());
    println!("  generation  = {}", ino.generation);
    println!("  ctime/mtime = {} / {}", ino.ctime, ino.mtime);

    assert!(
        ino.is_dir(),
        "root inode should be a directory (mode=0o{:o})",
        ino.mode
    );
    assert!(ino.links_count > 0, "root should have >= 1 link");
    assert!(ino.size > 0, "root dir size should be > 0");
    assert!(
        ino.has_extents(),
        "modern ext4 formatter root must use extents"
    );
    assert!(!ino.is_file());
    assert!(!ino.is_symlink());
}
