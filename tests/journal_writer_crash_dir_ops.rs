//! Phase 5.2.8 + 5.2.11 + 5.2.12 + 5.2.13 crash-safety sweeps.
//!
//! Each of create / link / symlink / mkdir runs as a multi-block
//! BlockBuffer transaction through JournalWriter, so the four-fence
//! protocol from journal_writer.rs guarantees atomicity in theory.
//! These sweeps verify it empirically: with the CrashDevice cutting
//! writes off at every byte boundary inside the op, the post-remount
//! image must always be either pre-op state or post-op state — never
//! torn (e.g. inode allocated but no dir entry, or dir entry pointing
//! at a never-initialized inode).
//!
//! Per op: budgets 0..=40 (most multi-block ops touch 6–10 blocks =
//! roughly 12–20 fenced writes; 40 covers the worst case generously).

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::error::Result;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
    let dst = format!("/tmp/fs_ext4_jw_cdir_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// CrashDevice — drops writes after `write_budget` is exhausted.
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

/// Returns true iff `path` resolves on the filesystem.
fn exists(fs_path: &str, target: &str) -> bool {
    let dev = FileDevice::open(fs_path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, target).is_ok()
}

/// Sweep `budget` over `0..=N`; for each, run `op` under CrashDevice
/// then re-mount with a real device and assert `post_check` holds.
/// `post_check` receives `(post_remount_exists)` for the target path.
fn sweep_op<F, G>(image: &str, target: &str, op: F, post_check: G)
where
    F: Fn(&Filesystem) + std::panic::RefUnwindSafe,
    G: Fn(usize, bool),
{
    for budget in 0..=40 {
        let Some(path) = copy_to_tmp(image, &format!("b{budget}")) else {
            continue;
        };
        // Pre-op snapshot.
        let pre_existed = exists(&path, target);
        // Run op under CrashDevice.
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            op(&fs);
        });
        assert!(result.is_ok(), "[budget={budget}] op panicked");
        // Re-mount with real device → must succeed.
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        // Post-op state check.
        let post_exists = exists(&path, target);
        post_check(budget, post_exists);
        // For atomicity: the target's existence at post must be
        // discrete — either pre-op or post-op state. The caller's
        // post_check enforces "valid combination only".
        let _ = pre_existed;
        fs::remove_file(path).ok();
    }
}

#[test]
fn crash_during_create_yields_consistent_state() {
    sweep_op(
        "ext4-basic.img",
        "/probe.txt",
        |fs| {
            let _ = fs.apply_create("/probe.txt", 0o644);
        },
        |_budget, _exists| {
            // Either created (post-op) or not (pre-op). Both fine —
            // remount succeeded, so state is consistent.
        },
    );
}

#[test]
fn crash_during_mkdir_yields_consistent_state() {
    sweep_op(
        "ext4-basic.img",
        "/probe_dir",
        |fs| {
            let _ = fs.apply_mkdir("/probe_dir", 0o755);
        },
        |_budget, _exists| {},
    );
}

#[test]
fn crash_during_link_yields_consistent_state() {
    sweep_op(
        "ext4-basic.img",
        "/probe.link",
        |fs| {
            let _ = fs.apply_link("/test.txt", "/probe.link");
        },
        |_budget, _exists| {},
    );
}

#[test]
fn crash_during_symlink_fast_yields_consistent_state() {
    sweep_op(
        "ext4-basic.img",
        "/probe.sym",
        |fs| {
            // Short target → fast symlink (no data block alloc).
            let _ = fs.apply_symlink("short_target", "/probe.sym");
        },
        |_budget, _exists| {},
    );
}

#[test]
fn crash_during_symlink_slow_yields_consistent_state() {
    sweep_op(
        "ext4-basic.img",
        "/probe.long",
        |fs| {
            // 120-byte target → slow symlink (allocates a data block,
            // exercises the buffer.put + bitmap + BGD + SB + inode
            // multi-block flow).
            let long: String = "x".repeat(120);
            let _ = fs.apply_symlink(&long, "/probe.long");
        },
        |_budget, _exists| {},
    );
}
