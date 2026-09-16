//! A replay plan that names one bad destination writes nothing (#150).
//!
//! `journal_apply::apply` bounds-checked each destination inside the loop
//! that wrote it, so the entries before a refused one were already on disk:
//! the mount failed over a filesystem it had half replayed.
//!
//! Two traps the issue records, both avoided here. The destination is
//! filled with a pattern the journal block does not hold, so a write that
//! happened is visible. And the good entry is applied on its own first, as
//! a control, so "nothing changed" cannot mean "the entry never applied".

use fs_ext4::block_io::BlockDevice;
use fs_ext4::error::{Error, Result};
use fs_ext4::fs::Filesystem;
use fs_ext4::inode::Inode;
use fs_ext4::journal::{ReplayEntry, ReplayPlan};
use fs_ext4::{journal_apply, mkfs};
use std::sync::{Arc, Mutex};

const BLOCK_SIZE: u32 = 4096;
const IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const PATTERN: u8 = 0xCC;

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

/// A formatted volume with a journal, and the destination block prefilled.
fn volume() -> (Arc<MemDev>, Filesystem, u64) {
    let dev = Arc::new(MemDev {
        bytes: Mutex::new(vec![0u8; IMAGE_BYTES as usize]),
    });
    mkfs::format_filesystem_with_flavor(
        dev.as_ref(),
        Some("REPLAY"),
        None,
        IMAGE_BYTES,
        BLOCK_SIZE,
        fs_ext4::features::FsFlavor::Ext3,
    )
    .expect("format");
    let fs = Filesystem::mount(dev.clone()).expect("mount");
    assert_ne!(
        fs.sb.journal_inode, 0,
        "fixture: the volume needs a journal"
    );
    let dest = fs.sb.blocks_count - 2;
    dev.write_at(
        dest * u64::from(BLOCK_SIZE),
        &[PATTERN; BLOCK_SIZE as usize],
    )
    .unwrap();
    (dev, fs, dest)
}

fn block(dev: &MemDev, n: u64) -> Vec<u8> {
    let mut buf = vec![0u8; BLOCK_SIZE as usize];
    dev.read_at(n * u64::from(BLOCK_SIZE), &mut buf).unwrap();
    buf
}

fn entry(fs_block: u64) -> ReplayEntry {
    ReplayEntry {
        transaction: 1,
        fs_block,
        journal_block: 1,
        flags: 0,
    }
}

fn plan(writes: Vec<ReplayEntry>) -> ReplayPlan {
    ReplayPlan {
        writes,
        ..ReplayPlan::default()
    }
}

#[test]
fn the_good_entry_applies_on_its_own() {
    let (dev, fs, dest) = volume();
    assert_eq!(
        journal_apply::apply(&fs, &plan(vec![entry(dest)])).unwrap(),
        1
    );
    assert!(
        block(&dev, dest) != vec![PATTERN; BLOCK_SIZE as usize],
        "control: the journal block must differ from the pattern, or a write is invisible"
    );
}

#[test]
fn one_bad_destination_refuses_the_plan_before_anything_is_written() {
    type Bad = fn(&Filesystem) -> ReplayEntry;
    let cases: [(&str, Bad, &str); 3] = [
        ("far past the end", |_| entry(u64::MAX / 2), "past the end"),
        (
            "one block past the end",
            |fs| ReplayEntry {
                fs_block: fs.sb.blocks_count,
                ..entry(0)
            },
            "past the end",
        ),
        (
            // Inside the range the journal inode's block map can address,
            // but past the blocks it holds, so the mapper answers "not
            // mapped" rather than refusing the number itself (Greptile on
            // #197: `u32::MAX` never reached that branch).
            "a source past the journal's last block",
            |fs| {
                let jinode =
                    Inode::parse(&fs.read_inode_raw(fs.sb.journal_inode).unwrap()).unwrap();
                ReplayEntry {
                    journal_block: jinode.size / u64::from(BLOCK_SIZE),
                    ..entry(3)
                }
            },
            "journal block unmapped",
        ),
    ];
    for (name, bad, why) in cases {
        let (dev, fs, dest) = volume();
        let before = block(&dev, dest);
        let bad = bad(&fs);
        let result = journal_apply::apply(&fs, &plan(vec![entry(dest), bad]));
        match &result {
            Err(Error::Corrupt(m)) => assert!(m.contains(why), "{name}: refused as {m:?}"),
            other => panic!("{name}: the plan must be refused, got {other:?}"),
        }
        assert!(
            block(&dev, dest) == before,
            "{name}: the entry before the refused one was already written"
        );
    }
}
