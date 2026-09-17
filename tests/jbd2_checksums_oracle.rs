//! JBD2 transaction checksums, judged by e2fsprogs from both sides (#80, #81).
//!
//! `mkfs.ext4` gives a `metadata_csum` filesystem a CSUM_V3 journal. Its
//! recovery code (shared with the kernel) checks every tag, the descriptor
//! and revoke tails and the commit block, and ends the log at a commit that
//! fails.
//!
//! - A transaction this crate commits, cut off after the journal is marked
//!   dirty, must be replayed by `e2fsck`. With the checksums left zero it
//!   stopped at the commit and the writes never landed.
//! - A transaction `debugfs` writes (several tags, a UUID after the first,
//!   a revoke block) must be replayed by this crate, and not replayed once a
//!   journalled data block or the commit block is damaged.
//!
//! Skips when e2fsprogs is not installed.

#![cfg(unix)]

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::error::Result;
use fs_ext4::inode::Inode;
use fs_ext4::journal_writer::JournalWriter;
use fs_ext4::Filesystem;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const BS: u64 = 4096;
/// Free blocks on a fresh 64 MiB image, well past group 0's metadata.
const TARGETS: [u64; 3] = [9000, 9001, 9002];
const REVOKED: u64 = 9100;

fn tool(name: &str) -> Option<String> {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|p| std::path::Path::new(p).exists())
}

fn run(program: &str, args: &[&str], stdin: Option<&str>) -> (Option<i32>, String) {
    use std::io::Write;
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    let input = stdin.unwrap_or("").to_owned();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        format!(
            "{program} {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn fresh_image(tag: &str, features: &str) -> Option<String> {
    let mkfs = tool("mkfs.ext4")?;
    tool("e2fsck")?;
    tool("debugfs")?;
    let image =
        fs_ext4_test_support::temp_path!("fs_ext4_jbd2_csum_{tag}_{}.img", std::process::id());
    std::fs::File::create(&image)
        .and_then(|f| f.set_len(64 * 1024 * 1024))
        .unwrap();
    let (code, log) = run(
        &mkfs,
        &["-q", "-F", "-b", "4096", "-O", features, &image],
        None,
    );
    assert_eq!(code, Some(0), "{log}");
    Some(image)
}

fn pattern(i: usize) -> Vec<u8> {
    (0..BS as usize)
        .map(|j| (0x40 + i as u8 * 0x10).wrapping_add((j % 251) as u8))
        .collect()
}

fn read_block(image: &str, block: u64) -> Vec<u8> {
    let dev = FileDevice::open(image).unwrap();
    let mut buf = vec![0u8; BS as usize];
    dev.read_at(block * BS, &mut buf).unwrap();
    buf
}

fn incompat_features(image: &str) -> String {
    let (_, log) = run(&tool("dumpe2fs").unwrap(), &["-h", image], None);
    log.lines()
        .filter(|l| l.starts_with("Journal features:"))
        .collect()
}

/// Drops every write after the first `budget`: a power cut.
struct CrashDevice {
    inner: Arc<dyn BlockDevice>,
    budget: usize,
    writes: AtomicUsize,
}

impl BlockDevice for CrashDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_at(offset, buf)
    }
    fn size_bytes(&self) -> u64 {
        self.inner.size_bytes()
    }
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        if self.writes.fetch_add(1, Ordering::SeqCst) >= self.budget {
            return Ok(());
        }
        self.inner.write_at(offset, buf)
    }
    fn flush(&self) -> Result<()> {
        self.inner.flush()
    }
    fn is_writable(&self) -> bool {
        self.inner.is_writable()
    }
}

/// OR `bits` into the journal superblock's `s_feature_incompat` and redo
/// its checksum. `mkfs.ext4` leaves a new journal's features empty and the
/// kernel sets them at the first mount; this is that step.
fn set_journal_incompat(image: &str, bits: u32) {
    let fs = Filesystem::mount(Arc::new(FileDevice::open(image).unwrap())).unwrap();
    let jinode = Inode::parse(&fs.read_inode_raw(fs.sb.journal_inode).unwrap()).unwrap();
    let phys = fs_ext4::jbd2::journal_block_to_physical(&fs, &jinode, 0)
        .unwrap()
        .unwrap();
    drop(fs);
    let dev = FileDevice::open_rw(image).unwrap();
    let mut jsb = vec![0u8; 1024];
    dev.read_at(phys * BS, &mut jsb).unwrap();
    let incompat = u32::from_be_bytes(jsb[0x28..0x2C].try_into().unwrap());
    jsb[0x28..0x2C].copy_from_slice(&(incompat | bits).to_be_bytes());
    jsb[0x50] = 4; // JBD2_CRC32C_CHKSUM
    jsb[0xFC..0x100].fill(0);
    let csum = fs_ext4::checksum::linux_crc32c(!0, &jsb);
    jsb[0xFC..0x100].copy_from_slice(&csum.to_be_bytes());
    dev.write_at(phys * BS, &jsb).unwrap();
    dev.flush().unwrap();
}

fn e2fsck_replays_what_this_crate_committed(tag: &str, features: &str, bits: u32, journal: &str) {
    let Some(image) = fresh_image(tag, features) else {
        eprintln!("skip: e2fsprogs not installed");
        return;
    };
    set_journal_incompat(&image, bits);
    assert!(
        incompat_features(&image).contains(journal),
        "[{tag}] {}",
        incompat_features(&image)
    );
    {
        let fs = Filesystem::mount(Arc::new(FileDevice::open(&image).unwrap())).unwrap();
        let mut writer = JournalWriter::open(&fs).unwrap().expect("a journal");
        let mut tx = writer.begin();
        for (i, &block) in TARGETS.iter().enumerate() {
            tx.add_write(block, pattern(i)).unwrap();
        }
        // Descriptor, three data blocks and the commit, then the journal
        // superblock marked dirty. The final-location writes are lost.
        let dev = CrashDevice {
            inner: Arc::new(FileDevice::open_rw(&image).unwrap()),
            budget: 5 + 1,
            writes: AtomicUsize::new(0),
        };
        writer.commit(&dev, &tx).unwrap();
    }
    for &block in &TARGETS {
        assert_eq!(
            read_block(&image, block),
            vec![0u8; BS as usize],
            "[{tag}] cut too late"
        );
    }

    let e2fsck = tool("e2fsck").unwrap();
    let (code, log) = run(&e2fsck, &["-fy", &image], None);
    assert!(matches!(code, Some(0 | 1)), "[{tag}] {log}");
    for (i, &block) in TARGETS.iter().enumerate() {
        assert!(
            read_block(&image, block) == pattern(i),
            "[{tag}] e2fsck did not replay block {block} of the crate's transaction: {log}"
        );
    }
    let (code, log) = run(&e2fsck, &["-fn", &image], None);
    assert_eq!(code, Some(0), "[{tag}] {log}");
    let _ = std::fs::remove_file(&image);
}

#[test]
fn e2fsck_replays_a_csum_v3_64bit_transaction() {
    // REVOKE | 64BIT | CSUM_V3
    e2fsck_replays_what_this_crate_committed(
        "v3_64",
        "metadata_csum,64bit",
        0x1 | 0x2 | 0x10,
        "journal_64bit journal_checksum_v3",
    );
}

/// Without 64BIT a CSUM_V3 tag is still 16 bytes; the writer laid 12.
#[test]
fn e2fsck_replays_a_csum_v3_32bit_transaction() {
    e2fsck_replays_what_this_crate_committed(
        "v3_32",
        "metadata_csum,^64bit",
        0x1 | 0x10,
        "journal_incompat_revoke journal_checksum_v3",
    );
}

/// A dirty journal written by `debugfs`: one transaction logging `TARGETS`
/// and revoking `REVOKED`.
fn debugfs_journal(tag: &str) -> Option<String> {
    let image = fresh_image(tag, "metadata_csum,64bit")?;
    let data = format!("{image}.data");
    let bytes: Vec<u8> = (0..TARGETS.len()).flat_map(pattern).collect();
    std::fs::write(&data, bytes).unwrap();
    let blocks = TARGETS.map(|b| b.to_string()).join(",");
    let script = format!("jo -c\njw -b {blocks} -r {REVOKED} {data}\njc\n");
    let (code, log) = run(
        &tool("debugfs").unwrap(),
        &["-w", "-f", "-", &image],
        Some(&script),
    );
    let _ = std::fs::remove_file(&data);
    assert!(code == Some(0) && log.contains("Setting csum v3"), "{log}");
    Some(image)
}

/// Flip one byte of journal block `journal_block` at `offset`.
fn damage_journal(image: &str, journal_block: u64, offset: u64) {
    let fs = Filesystem::mount(Arc::new(FileDevice::open(image).unwrap())).unwrap();
    let jinode = Inode::parse(&fs.read_inode_raw(fs.sb.journal_inode).unwrap()).unwrap();
    let phys = fs_ext4::jbd2::journal_block_to_physical(&fs, &jinode, journal_block)
        .unwrap()
        .unwrap();
    drop(fs);
    let dev = FileDevice::open_rw(image).unwrap();
    let mut byte = [0u8; 1];
    dev.read_at(phys * BS + offset, &mut byte).unwrap();
    byte[0] ^= 0x5A;
    dev.write_at(phys * BS + offset, &byte).unwrap();
    dev.flush().unwrap();
}

#[test]
fn a_debugfs_transaction_is_replayed() {
    let Some(image) = debugfs_journal("replayed") else {
        eprintln!("skip: e2fsprogs not installed");
        return;
    };
    Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap())).expect("mount replays");
    for (i, &block) in TARGETS.iter().enumerate() {
        assert!(
            read_block(&image, block) == pattern(i),
            "block {block} of debugfs's transaction was not replayed"
        );
    }
    let _ = std::fs::remove_file(&image);
}

/// Journal block 1 is the descriptor, 2..=4 the data, 5 the revoke block
/// and 6 the commit. A committed transaction with a damaged data block is
/// corruption: nothing is replayed.
#[test]
fn a_damaged_data_block_is_refused() {
    let Some(image) = debugfs_journal("data") else {
        eprintln!("skip: e2fsprogs not installed");
        return;
    };
    damage_journal(&image, 3, 100);
    let err = Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap()))
        .err()
        .expect("replay refuses a data block failing its tag checksum");
    assert!(matches!(err, fs_ext4::Error::BadChecksum { .. }), "{err:?}");
    for &block in &TARGETS {
        assert_eq!(read_block(&image, block), vec![0u8; BS as usize]);
    }
    let _ = std::fs::remove_file(&image);
}

/// A commit block failing its checksum is where a crash ended the log: the
/// transaction is not replayed and the mount goes ahead.
#[test]
fn a_damaged_commit_block_ends_the_log() {
    let Some(image) = debugfs_journal("commit") else {
        eprintln!("skip: e2fsprogs not installed");
        return;
    };
    damage_journal(&image, 6, 100);
    Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap()))
        .expect("a torn commit is the end of the log, not an error");
    for &block in &TARGETS {
        assert_eq!(read_block(&image, block), vec![0u8; BS as usize]);
    }
    let _ = std::fs::remove_file(&image);
}

/// A damaged descriptor tail ends the log the same way.
#[test]
fn a_damaged_descriptor_block_ends_the_log() {
    let Some(image) = debugfs_journal("descriptor") else {
        eprintln!("skip: e2fsprogs not installed");
        return;
    };
    damage_journal(&image, 1, 4000);
    Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap()))
        .expect("a torn descriptor is the end of the log, not an error");
    for &block in &TARGETS {
        assert_eq!(read_block(&image, block), vec![0u8; BS as usize]);
    }
    let _ = std::fs::remove_file(&image);
}

/// ASYNC_COMMIT, set in the journal superblock with its checksum redone:
/// replay refuses rather than walks.
#[test]
fn an_unsupported_journal_feature_is_refused() {
    let Some(image) = debugfs_journal("async") else {
        eprintln!("skip: e2fsprogs not installed");
        return;
    };
    set_journal_incompat(&image, 0x4);

    let err = Filesystem::mount(Arc::new(FileDevice::open_rw(&image).unwrap()))
        .err()
        .expect("replay refuses an ASYNC_COMMIT journal");
    assert!(matches!(err, fs_ext4::Error::Unsupported(_)), "{err:?}");
    for &block in &TARGETS {
        assert_eq!(read_block(&image, block), vec![0u8; BS as usize]);
    }
    let _ = std::fs::remove_file(&image);
}
