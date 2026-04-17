//! End-to-end: parse the existing test-disks/ext4-basic.img superblock.

use ext4rs::block_io::FileDevice;
use ext4rs::superblock::Superblock;
use ext4rs::error::Error;

const TEST_IMAGE: &str = "test-disks/ext4-basic.img";

#[test]
fn superblock_parses_basic_image() {
    let dev = FileDevice::open(TEST_IMAGE).expect("open test image");
    let sb = Superblock::read(&dev).expect("parse superblock");

    assert_eq!(sb.magic, 0xEF53, "magic");
    assert!(sb.block_size() == 1024 || sb.block_size() == 2048
            || sb.block_size() == 4096, "block_size = {}", sb.block_size());
    assert!(sb.inodes_count > 0, "inodes_count");
    assert!(sb.blocks_count > 0, "blocks_count");
    assert!(sb.blocks_per_group > 0, "blocks_per_group");
    assert!(sb.inodes_per_group > 0, "inodes_per_group");

    println!("ext4 superblock parsed:");
    println!("  volume_name = {:?}", sb.volume_name);
    println!("  uuid        = {:02x?}", sb.uuid);
    println!("  block_size  = {}", sb.block_size());
    println!("  inodes      = {} ({} free)", sb.inodes_count, sb.free_inodes_count);
    println!("  blocks      = {} ({} free)", sb.blocks_count, sb.free_blocks_count);
    println!("  groups      = {}", sb.block_group_count());
    println!("  rev_level   = {}", sb.rev_level);
    println!("  inode_size  = {}", sb.inode_size);
    println!("  features:");
    println!("    compat    = 0x{:08x}", sb.feature_compat);
    println!("    incompat  = 0x{:08x}", sb.feature_incompat);
    println!("    ro_compat = 0x{:08x}", sb.feature_ro_compat);
}

#[test]
fn rejects_non_ext4_data() {
    use std::io::Write;
    let mut tmp = std::env::temp_dir();
    tmp.push("ext4rs_bad_magic.img");
    {
        let mut f = std::fs::File::create(&tmp).unwrap();
        // Write 2048 bytes of zeroes — no magic at offset 1024+0x38
        f.write_all(&vec![0u8; 2048]).unwrap();
    }
    let dev = FileDevice::open(tmp.to_str().unwrap()).unwrap();
    let result = Superblock::read(&dev);
    assert!(matches!(result, Err(Error::BadMagic { .. })));
    let _ = std::fs::remove_file(&tmp);
}
