//! A journal superblock cannot declare more blocks than its inode holds
//! (#182).
//!
//! `JournalWriter::open` bounded `s_maxlen` only by `s_blocks_count`, which
//! is the image's own claim, and then reserved `s_maxlen` entries before
//! mapping one. The kernel's `jbd2` compares `s_maxlen` with the journal
//! inode's length instead ("journal file too short"), and a journal also
//! cannot be longer than the device it is on.

use fs_ext4::block_io::BlockDevice;
use fs_ext4::error::{Error, Result};
use fs_ext4::fs::Filesystem;
use fs_ext4::inode::Inode;
use fs_ext4::journal_writer::JournalWriter;
use fs_ext4::{jbd2, mkfs};
use std::sync::{Arc, Mutex};

const BLOCK_SIZE: u32 = 4096;
const IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// `s_maxlen`, big-endian, in the JBD2 superblock.
const MAXLEN_AT: u64 = 0x10;

struct MemDev {
    bytes: Mutex<Vec<u8>>,
}

impl BlockDevice for MemDev {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let b = self.bytes.lock().unwrap();
        let start = offset as usize;
        buf.copy_from_slice(&b[start..start + buf.len()]);
        Ok(())
    }
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        let mut b = self.bytes.lock().unwrap();
        let start = offset as usize;
        b[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        IMAGE_BYTES
    }
    fn is_writable(&self) -> bool {
        true
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
}

/// Mount a journaled volume whose JBD2 superblock declares
/// `max_len(inode_blocks)`. A writable mount opens the journal writer, so
/// this is where the bound is met.
fn mount_with_maxlen(max_len: impl Fn(u64) -> u32) -> Result<Filesystem> {
    let dev = Arc::new(MemDev {
        bytes: Mutex::new(vec![0u8; IMAGE_BYTES as usize]),
    });
    mkfs::format_filesystem_with_flavor(
        dev.as_ref(),
        None,
        None,
        IMAGE_BYTES,
        BLOCK_SIZE,
        fs_ext4::features::FsFlavor::Ext3,
    )
    .expect("format");
    let fs = Filesystem::mount(dev.clone()).expect("mount");
    let jinode = Inode::parse(&fs.read_inode_raw(fs.sb.journal_inode).unwrap()).unwrap();
    let inode_blocks = jinode.size / u64::from(BLOCK_SIZE);
    assert!(inode_blocks > 0, "fixture: the journal inode has a size");
    let jsb_block = jbd2::journal_block_to_physical(&fs, &jinode, 0)
        .unwrap()
        .unwrap();
    dev.write_at(
        jsb_block * u64::from(BLOCK_SIZE) + MAXLEN_AT,
        &max_len(inode_blocks).to_be_bytes(),
    )
    .unwrap();
    Filesystem::mount(dev)
}

#[test]
fn the_journal_as_formatted_opens() {
    let fs = mount_with_maxlen(|blocks| blocks as u32).expect("mount");
    assert!(JournalWriter::open(&fs).expect("open").is_some());
}

#[test]
fn a_maxlen_past_the_journal_inode_is_refused_by_that_bound() {
    for (name, max_len) in [
        ("one past the inode", (|b| b as u32 + 1) as fn(u64) -> u32),
        ("0xFFFFFFFF", |_| u32::MAX),
    ] {
        match mount_with_maxlen(max_len) {
            Err(Error::Corrupt(why)) => assert!(
                why.contains("than its inode holds") || why.contains("than the filesystem holds"),
                "{name}: refused, but not by a bound on the journal's length: {why}"
            ),
            Err(other) => panic!("{name}: wrong error {other:?}"),
            Ok(_) => panic!("{name}: a journal longer than its inode was opened"),
        }
    }
}
