//! Round-trip: format an in-memory volume with `mkfs::format_filesystem`,
//! then mount it via the existing read path and verify the root directory
//! is readable + the root inode looks sane.

use fs_ext4::block_io::BlockDevice;
use fs_ext4::dir;
use fs_ext4::error::Result;
use fs_ext4::extent;
use fs_ext4::fs::Filesystem;
use fs_ext4::inode::S_IFDIR;
use fs_ext4::mkfs;
use fs_ext4::superblock::EXT4_MAGIC;
use std::sync::{Arc, Mutex};

/// In-memory R/W block device backed by a single Vec<u8>.
struct MemDev {
    bytes: Mutex<Vec<u8>>,
    size: u64,
}

impl MemDev {
    fn new(size: u64) -> Arc<Self> {
        Arc::new(Self {
            bytes: Mutex::new(vec![0u8; size as usize]),
            size,
        })
    }
}

impl BlockDevice for MemDev {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let b = self.bytes.lock().unwrap();
        let start = offset as usize;
        let end = start + buf.len();
        assert!(end <= b.len(), "read past EOF");
        buf.copy_from_slice(&b[start..end]);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.size
    }
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        let mut b = self.bytes.lock().unwrap();
        let start = offset as usize;
        let end = start + buf.len();
        assert!(end <= b.len(), "write past EOF");
        b[start..end].copy_from_slice(buf);
        Ok(())
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
    fn is_writable(&self) -> bool {
        true
    }
}

#[test]
fn mkfs_then_mount_yields_empty_root() {
    let size: u64 = 32 * 1024 * 1024; // 32 MiB
    let block_size: u32 = 4096;
    let dev = MemDev::new(size);

    mkfs::format_filesystem(
        dev.as_ref(),
        Some("DJTEST"),
        Some([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x10, 0x32, 0x54, 0x76, 0x98, 0xBA,
            0xDC, 0xFE,
        ]),
        size,
        block_size,
    )
    .expect("format_filesystem");

    // Mount via the same Vec.
    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount fresh fs");

    // Volume name survived.
    assert_eq!(fs.sb.volume_name, "DJTEST", "label round-trip");
    assert_eq!(fs.sb.block_size(), block_size, "block size round-trip");
    assert!(fs.csum.enabled, "metadata_csum should be on");
    assert!(fs.sb.is_clean(), "fresh FS must be marked clean");

    // Root inode (ino 2): mode 0o40755, type=dir, links=2.
    let (root, _raw) = fs
        .read_inode_verified(2)
        .expect("root inode parses + verifies");
    assert!(root.is_dir(), "root must be a directory");
    assert_eq!(root.mode & S_IFDIR, S_IFDIR);
    assert_eq!(root.mode & 0o7777, 0o755, "root permission bits");
    assert_eq!(root.links_count, 2, "root link count = 2 for `.`+`..`");
    assert!(root.has_extents(), "root must use extents");

    // Walk the root directory: expect exactly `.` and `..`.
    let bs = fs.sb.block_size();
    let phys = extent::map_logical(&root.block, dev_dyn.as_ref(), bs, 0)
        .expect("map_logical")
        .expect("root dir block 0 is mapped");
    let mut block = vec![0u8; bs as usize];
    dev_dyn
        .read_at(phys * bs as u64, &mut block)
        .expect("read root dir block");

    assert!(
        dir::has_csum_tail(&block),
        "root dir block should carry the csum tail"
    );
    assert!(
        fs.csum.verify_dir_entry_tail(2, root.generation, &block),
        "root dir block tail csum must verify"
    );

    let entries = dir::parse_block(&block, /* has_filetype */ true).expect("parse root dir");
    let names: Vec<Vec<u8>> = entries.iter().map(|e| e.name.clone()).collect();
    assert!(names.iter().any(|n| n == b"."), "root dir missing `.`");
    assert!(names.iter().any(|n| n == b".."), "root dir missing `..`");
    assert_eq!(
        entries.len(),
        2,
        "expected only `.` and `..`, got {names:?}",
    );
    for e in &entries {
        assert_eq!(e.inode, 2, "both `.` and `..` point to root inode 2");
        assert_eq!(e.file_type, dir::DirEntryType::Directory);
    }
}

#[test]
fn mkfs_multi_group_reads_root() {
    let size: u64 = 256 * 1024 * 1024;
    let block_size: u32 = 4096;
    let dev = MemDev::new(size);

    mkfs::format_filesystem(dev.as_ref(), Some("MULTIGRP"), None, size, block_size)
        .expect("format failed");

    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount failed");

    assert_eq!(
        fs.sb.volume_name, "MULTIGRP",
        "filesystem label is malformed"
    );
    assert_eq!(fs.sb.block_size(), block_size);
    assert!(
        fs.sb.block_group_count() >= 2,
        "expected >= 2 block groups, got {}",
        fs.sb.block_group_count()
    );
    assert!(fs.csum.enabled, "fs.csum.enabled is not enabled");
    assert!(fs.sb.is_clean(), "fs.sb is not clean");

    let bpg = u64::from(fs.sb.blocks_per_group);
    let mut sb_blk = vec![0u8; block_size as usize];
    dev_dyn
        .read_at(bpg * block_size as u64, &mut sb_blk)
        .expect("reading group 1 backup superblock failed");
    assert_eq!(
        u16::from_le_bytes([sb_blk[0x38], sb_blk[0x39]]),
        EXT4_MAGIC,
        "group 1 must hold a backup superblock"
    );
    assert_eq!(
        u16::from_le_bytes([sb_blk[0x5A], sb_blk[0x5B]]),
        1,
        "backup superblock s_block_group_nr must equal its group"
    );

    let (root, _raw) = fs.read_inode_verified(2).expect("root inode");
    assert!(root.is_dir(), "root must be a directory");
    assert_eq!(root.mode & S_IFDIR, S_IFDIR);
    assert_eq!(root.mode & 0o7777, 0o755, "invalid root permissions");
    assert_eq!(root.links_count, 2, "invalid root link count (!= 2)");
    assert!(root.has_extents(), "root must have extents");

    let bs = fs.sb.block_size();
    let Some(phys) =
        extent::map_logical(&root.block, dev_dyn.as_ref(), bs, 0).expect("extent mapping failed")
    else {
        panic!("extent returned unexpected sparse hole.");
    };

    let mut block = vec![0u8; bs as usize];
    dev_dyn
        .read_at(phys * bs as u64, &mut block)
        .expect("reading root directory block failed");
    assert!(
        dir::has_csum_tail(&block),
        "root dir block should carry the csum tail"
    );
    assert!(
        fs.csum.verify_dir_entry_tail(2, root.generation, &block),
        "fs.csum does not verify"
    );
    let entries = dir::parse_block(&block, true).expect("parsing root directory block failed");
    assert_eq!(entries.len(), 2, "expected only `.` and `..`");
    assert_eq!(entries[0].name, b".", "first entry must be `.`");
    assert_eq!(entries[1].name, b"..", "second entry must be `..`");
}
