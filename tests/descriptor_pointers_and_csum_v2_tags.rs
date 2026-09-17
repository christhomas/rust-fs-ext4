//! The rest of #64's two fixes, pinned (#184).
//!
//! **Descriptor pointers.** `bgd::read_all` refuses a block bitmap, inode
//! bitmap or inode table at or past `blocks_count`, and an inode table whose
//! extent runs past it. The existing test patched the block bitmap and the
//! inode table's low half only. Here every field is covered, including the
//! `_hi` halves and an inode table whose start is in range and whose end is
//! not, with the edge on each side.
//!
//! **CSUM_V2 tags.** `JournalWriter::begin` asks for v3 tags only on a
//! CSUM_V3 journal. When it asked "any checksums", a CSUM_V2 journal got
//! 16-byte v3 tags where readers parse classic ones, and every tag after the
//! first replayed onto the wrong block. Here a two-block transaction is
//! committed to a CSUM_V2 journal and walked back, and the SECOND
//! destination is checked.
//!
//! Volumes come from `mkfs.ext4`; fails without e2fsprogs (`chore tools`).

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::fs::Filesystem;
use fs_ext4::inode::Inode;
use fs_ext4::journal_writer::JournalWriter;
use fs_ext4::{jbd2, journal};
use std::process::Command;
use std::sync::Arc;

fn mkfs(tag: &str, features: &str) -> String {
    let mkfs = fs_ext4_test_support::oracle_tool("mkfs.ext4");
    let path = fs_ext4_test_support::temp_path!("fs_ext4_184_{tag}_{}.img", std::process::id());
    std::fs::File::create(&path)
        .and_then(|f| f.set_len(64 * 1024 * 1024))
        .unwrap();
    let out = Command::new(mkfs)
        .args(["-q", "-F", "-b", "4096", "-O", features])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

// ---------------------------------------------------------------------------
// Descriptor pointers
// ---------------------------------------------------------------------------

/// `ext4_group_desc` offsets, low and high halves.
const BLOCK_BITMAP: (u64, u64) = (0x00, 0x20);
const INODE_BITMAP: (u64, u64) = (0x04, 0x24);
const INODE_TABLE: (u64, u64) = (0x08, 0x28);

/// Set group 0's `field` to `value`, both halves. No descriptor checksum to
/// restamp: the volume has neither metadata_csum nor uninit_bg.
fn set_descriptor(path: &str, field: (u64, u64), value: u64) {
    let (table_at, _) = descriptor_geometry(path);
    let dev = FileDevice::open_rw(path).unwrap();
    dev.write_at(table_at + field.0, &(value as u32).to_le_bytes())
        .unwrap();
    dev.write_at(table_at + field.1, &((value >> 32) as u32).to_le_bytes())
        .unwrap();
    dev.flush().unwrap();
}

/// (byte offset of the descriptor table, blocks in group 0's inode table).
fn descriptor_geometry(path: &str) -> (u64, u64) {
    let fs = Filesystem::mount(Arc::new(FileDevice::open(path).unwrap())).unwrap();
    let bs = u64::from(fs.sb.block_size());
    let table = (u64::from(fs.sb.first_data_block) + 1) * bs;
    let blocks = (u64::from(fs.sb.inodes_per_group) * u64::from(fs.sb.inode_size)).div_ceil(bs);
    (table, blocks)
}

fn mount_result(path: &str) -> Result<(), String> {
    Filesystem::mount(Arc::new(FileDevice::open(path).unwrap()))
        .map(|_| ())
        .map_err(|e| format!("{e}"))
}

#[test]
fn every_descriptor_pointer_is_bounded_including_the_high_halves() {
    let base = mkfs("desc", "^metadata_csum,^uninit_bg,^has_journal,64bit");
    let blocks_count = Filesystem::mount(Arc::new(FileDevice::open(&base).unwrap()))
        .unwrap()
        .sb
        .blocks_count;
    let (_, table_blocks) = descriptor_geometry(&base);

    let case = |name: &str, field: (u64, u64), value: u64, refused: bool| {
        let path = format!("{base}.{}", name.replace(' ', "_"));
        std::fs::copy(&base, &path).unwrap();
        set_descriptor(&path, field, value);
        match (mount_result(&path), refused) {
            (Err(why), true) => {
                assert!(why.contains("outside the filesystem"), "{name}: {why}")
            }
            (Ok(()), false) => {}
            (got, _) => panic!("{name} (value {value:#x}): {got:?}"),
        }
        std::fs::remove_file(&path).ok();
    };

    case("inode bitmap at the end", INODE_BITMAP, blocks_count, true);
    case(
        "inode bitmap last block",
        INODE_BITMAP,
        blocks_count - 1,
        false,
    );
    case("block bitmap high half", BLOCK_BITMAP, 1 << 32, true);
    case("inode bitmap high half", INODE_BITMAP, 1 << 33, true);
    case("inode table high half", INODE_TABLE, 1 << 32, true);
    case(
        "inode table ending one past",
        INODE_TABLE,
        blocks_count - table_blocks + 1,
        true,
    );
    case(
        "inode table ending at the end",
        INODE_TABLE,
        blocks_count - table_blocks,
        false,
    );
    std::fs::remove_file(&base).ok();
}

// ---------------------------------------------------------------------------
// CSUM_V2 tags
// ---------------------------------------------------------------------------

#[test]
fn a_csum_v2_journal_replays_every_tag_onto_its_own_block() {
    let path = mkfs("csumv2", "metadata_csum");
    // Give the journal CSUM_V2 (s_feature_incompat, big-endian at 0x28 of
    // the journal superblock) with the crc32c checksum type (0x50) that V2
    // and V3 both use. mkfs writes a journal with no checksum feature; the
    // kernel adds one at mount when asked to.
    {
        let fs = Filesystem::mount(Arc::new(FileDevice::open(&path).unwrap())).unwrap();
        let jinode = Inode::parse(&fs.read_inode_raw(fs.sb.journal_inode).unwrap()).unwrap();
        let phys = jbd2::journal_block_to_physical(&fs, &jinode, 0)
            .unwrap()
            .unwrap();
        let jsb_at = phys * u64::from(fs.sb.block_size());
        let mut raw = [0u8; 4];
        fs.dev.read_at(jsb_at + 0x28, &mut raw).unwrap();
        let incompat = (u32::from_be_bytes(raw) & !0x10) | 0x08;
        drop(fs);
        let dev = FileDevice::open_rw(&path).unwrap();
        dev.write_at(jsb_at + 0x28, &incompat.to_be_bytes())
            .unwrap();
        dev.write_at(jsb_at + 0x50, &[4]).unwrap();
        dev.flush().unwrap();
    }

    // Commit through a device that loses every write after the journal
    // superblock is marked dirty: the state a crash between the journal and
    // the checkpoint leaves, where replay is what decides the outcome.
    struct CrashAfterJsb {
        inner: FileDevice,
        jsb_at: u64,
        crashed: std::sync::atomic::AtomicBool,
    }
    impl BlockDevice for CrashAfterJsb {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_ext4::Result<()> {
            self.inner.read_at(offset, buf)
        }
        fn size_bytes(&self) -> u64 {
            self.inner.size_bytes()
        }
        fn write_at(&self, offset: u64, buf: &[u8]) -> fs_ext4::Result<()> {
            use std::sync::atomic::Ordering;
            if self.crashed.load(Ordering::SeqCst) {
                return Ok(());
            }
            self.inner.write_at(offset, buf)?;
            if offset == self.jsb_at {
                self.crashed.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
        fn flush(&self) -> fs_ext4::Result<()> {
            self.inner.flush()
        }
        fn is_writable(&self) -> bool {
            true
        }
    }

    let (first, second) = {
        let fs = Filesystem::mount(Arc::new(FileDevice::open_rw(&path).unwrap())).unwrap();
        let jsb = jbd2::read_superblock(&fs).unwrap().unwrap();
        assert!(
            jsb.uses_csum_v2_or_v3() && !jsb.uses_csum_v3(),
            "fixture: CSUM_V2"
        );
        let bs = fs.sb.block_size() as usize;
        let jinode = Inode::parse(&fs.read_inode_raw(fs.sb.journal_inode).unwrap()).unwrap();
        let jsb_at = jbd2::journal_block_to_physical(&fs, &jinode, 0)
            .unwrap()
            .unwrap()
            * u64::from(fs.sb.block_size());
        let (first, second) = (fs.sb.blocks_count - 20, fs.sb.blocks_count - 19);
        let mut jw = JournalWriter::open(&fs).unwrap().expect("a journal");
        let mut tx = jw.begin();
        tx.add_write(first, vec![0xA1; bs]).unwrap();
        tx.add_write(second, vec![0xB2; bs]).unwrap();
        let crash = CrashAfterJsb {
            inner: FileDevice::open_rw(&path).unwrap(),
            jsb_at,
            crashed: std::sync::atomic::AtomicBool::new(false),
        };
        jw.commit(&crash, &tx).unwrap();
        (first, second)
    };

    // Walk what was committed, as a replay would, without applying it.
    struct ReadOnly(FileDevice);
    impl BlockDevice for ReadOnly {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_ext4::Result<()> {
            self.0.read_at(offset, buf)
        }
        fn size_bytes(&self) -> u64 {
            self.0.size_bytes()
        }
        fn is_writable(&self) -> bool {
            false
        }
    }
    let fs = Filesystem::mount(Arc::new(ReadOnly(FileDevice::open(&path).unwrap()))).unwrap();
    let jsb = jbd2::read_superblock(&fs).unwrap().unwrap();
    let plan = journal::walk(&fs, &jsb).expect("walk the journal");
    let targets: Vec<u64> = plan.writes.iter().map(|w| w.fs_block).collect();
    assert_eq!(
        targets,
        vec![first, second],
        "each tag must name its own destination; the second is where v3 tags on a \
         CSUM_V2 journal went wrong"
    );
    std::fs::remove_file(&path).ok();
}
