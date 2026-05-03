//! Phase 5.1.4: fault-injection probes for the JournalWriter four-fence
//! protocol.
//!
//! A `CrashDevice` wraps a real `BlockDevice` and silently drops writes
//! after a configurable budget is exhausted. This lets us simulate a
//! power-loss event at every byte boundary inside an inode op and verify
//! the next mount yields a consistent state — either the pre-op or the
//! post-op view, never a half-applied tear.
//!
//! Contract under test (from `journal_writer.rs` module docs):
//!
//! - Budget exhausted before step 2 (mark dirty): jsb still says clean,
//!   replay is a no-op, fs state == pre-op.
//! - Budget exhausted between step 2 and step 4: jsb says dirty, replay
//!   reads the transaction and applies the writes — fs state == post-op.
//! - Budget exhausted after step 4: clean, fs state == post-op.
//!
//! No budget should produce a state where the inode mode is half-modified
//! (impossible: chmod only mutates 2 bytes of a 256-byte inode; the
//! whole inode block is journaled atomically).

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::error::Result;
use fs_ext4::inode::S_IFMT;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Wraps another BlockDevice. After `write_budget` writes have completed,
/// further `write_at` and `flush` calls return Ok(()) without doing
/// anything. Reads always pass through. Models a power-loss event with
/// no torn writes (the kernel either flushes a sector cleanly or it
/// doesn't — we don't try to simulate sub-block tears here).
struct CrashDevice {
    inner: Arc<dyn BlockDevice>,
    write_budget: AtomicUsize,
    writes_attempted: AtomicUsize,
}

impl CrashDevice {
    fn new(inner: Arc<dyn BlockDevice>, write_budget: usize) -> Self {
        Self {
            inner,
            write_budget: AtomicUsize::new(write_budget),
            writes_attempted: AtomicUsize::new(0),
        }
    }

    fn writes_attempted(&self) -> usize {
        self.writes_attempted.load(Ordering::SeqCst)
    }
}

impl BlockDevice for CrashDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn size_bytes(&self) -> u64 {
        self.inner.size_bytes()
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        let n = self.writes_attempted.fetch_add(1, Ordering::SeqCst);
        let budget = self.write_budget.load(Ordering::SeqCst);
        if n >= budget {
            // Silently dropped — caller sees Ok(()) but bytes never reach disk.
            return Ok(());
        }
        self.inner.write_at(offset, buf)
    }

    fn flush(&self) -> Result<()> {
        // Flush is allowed to succeed even if subsequent writes are dropped;
        // a flush call doesn't itself dirty anything new.
        self.inner.flush()
    }

    fn is_writable(&self) -> bool {
        self.inner.is_writable()
    }
}

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, tag: &str) -> Option<String> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!("/tmp/fs_ext4_jw_crash_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn read_mode(path: &str) -> u16 {
    let dev = FileDevice::open(path).expect("open ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    let ino =
        fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt").expect("lookup");
    fs.read_inode_verified(ino).expect("read inode").0.mode
}

#[test]
fn crash_device_with_unlimited_budget_matches_real_device() {
    // Sanity: with budget = usize::MAX the wrapper is a transparent
    // pass-through and the chmod must persist exactly as it would
    // through a normal FileDevice.
    let Some(path) = copy_to_tmp("ext4-basic.img", "unlimited") else {
        return;
    };
    let original = read_mode(&path);
    let new_mode = 0o644u16;
    let file_type = original & S_IFMT;
    let expected = file_type | new_mode;

    {
        let inner = FileDevice::open_rw(&path).expect("rw");
        let dev = CrashDevice::new(Arc::new(inner), usize::MAX);
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        fs.apply_chmod("/test.txt", new_mode).expect("chmod");
    }

    assert_eq!(
        read_mode(&path),
        expected,
        "transparent pass-through should produce the post-chmod state"
    );
    fs::remove_file(path).ok();
}

#[test]
fn crash_at_zero_writes_leaves_pre_state_intact() {
    // budget=0: every write is dropped. The chmod call itself returns Ok
    // (writes silently no-op'd), but on remount nothing changed on disk.
    let Some(path) = copy_to_tmp("ext4-basic.img", "budget0") else {
        return;
    };
    let original = read_mode(&path);

    {
        let inner = FileDevice::open_rw(&path).expect("rw");
        let dev = CrashDevice::new(Arc::new(inner), 0);
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        // The op may succeed or fail — we don't care. We only care that
        // disk state is unchanged AND the post-mount jsb is consistent.
        let _ = fs.apply_chmod("/test.txt", 0o600);
    }

    assert_eq!(
        read_mode(&path),
        original,
        "with all writes dropped, on-disk inode mode must equal the original"
    );

    // The next mount must succeed (replay handles a dirty journal whose
    // commit block is missing — falls back to no-op).
    let dev = FileDevice::open_rw(&path).expect("rw remount");
    let _ = Filesystem::mount(Arc::new(dev)).expect("remount must not fail");
    fs::remove_file(path).ok();
}

#[test]
fn every_crash_budget_yields_consistent_state() {
    // The strong contract: regardless of WHEN we cut the writes off,
    // the resulting on-disk state always reflects EITHER the pre-op mode
    // OR the post-op mode — never a half-applied tear or an unmountable
    // image.
    //
    // Sweep budgets 0..=20 (covers all four protocol fences for a single-
    // inode chmod). Each iteration uses a fresh image copy.
    let Some(probe_path) = copy_to_tmp("ext4-basic.img", "probe") else {
        return;
    };
    let original = read_mode(&probe_path);
    let new_mode = 0o600u16;
    let file_type = original & S_IFMT;
    let post_mode = file_type | new_mode;
    fs::remove_file(probe_path).ok();

    for budget in 0..=20 {
        let Some(path) = copy_to_tmp("ext4-basic.img", &format!("b{budget}")) else {
            continue;
        };
        let writes_used;
        {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash.clone()).expect("mount");
            let _ = fs.apply_chmod("/test.txt", new_mode);
            // Drop fs to release the journal lock.
            drop(fs);
            writes_used = crash.writes_attempted();
        }

        // Re-mount with a real device; replay runs if the journal is dirty.
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let after = read_mode(&path);

        assert!(
            after == original || after == post_mode,
            "budget={budget}: inode mode is neither pre ({:o}) nor post ({:o}) — got {:o} \
             (writes attempted during crash run: {writes_used})",
            original,
            post_mode,
            after
        );
        fs::remove_file(path).ok();
    }
}
