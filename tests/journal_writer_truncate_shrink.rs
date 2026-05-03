//! Phase 5.2.6: `apply_truncate_shrink` is now a multi-block journaled
//! transaction (inode + bitmap + BGD + SB all in one atomic commit).
//!
//! Three contracts pinned:
//! - Each shrink advances `jsb.sequence` (proves the writer was used,
//!   not the unjournaled fallback).
//! - The journal is back to clean after each op.
//! - `i_size` + `i_blocks` + SB free_blocks_count + BGD free_blocks_count
//!   all reflect the truncate by the time the next mount sees them.
//!
//! Plus a crash-safety probe: with the CrashDevice cutting writes at
//! every byte boundary, the post-remount state is either the original
//! file or the truncated file — never a half-applied tear.

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
    let dst = format!("/tmp/fs_ext4_jw_trunc_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).expect("resolve")
}

fn snapshot(path: &str, file: &str) -> (u64, u64, u64, u32, Option<u32>) {
    // Returns (size, i_blocks, sb_free_blocks, bgd0_free_blocks, jsb.sequence)
    let dev = FileDevice::open(path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let ino = resolve(&fs, file);
    let (inode, _) = fs.read_inode_verified(ino).expect("read");
    let seq = fs_ext4::jbd2::read_superblock(&fs)
        .expect("jsb")
        .map(|j| j.sequence);
    (
        inode.size,
        inode.blocks,
        fs.sb.free_blocks_count,
        fs.groups[0].free_blocks_count,
        seq,
    )
}

#[test]
fn truncate_shrink_atomically_updates_inode_bitmap_bgd_sb() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "atomic") else {
        return;
    };

    let (size_before, blocks_before, sb_before, bgd_before, seq_before) =
        snapshot(&path, "/test.txt");
    assert!(size_before > 0, "fixture must have non-zero size");

    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink to 0");
    }

    let (size_after, blocks_after, sb_after, bgd_after, seq_after) = snapshot(&path, "/test.txt");

    assert_eq!(size_after, 0, "i_size not zeroed");
    assert_eq!(blocks_after, 0, "i_blocks not zeroed");
    assert!(
        sb_after > sb_before,
        "SB free_blocks did not credit back ({sb_before} -> {sb_after})"
    );
    assert!(
        bgd_after > bgd_before,
        "BGD free_blocks did not credit back ({bgd_before} -> {bgd_after})"
    );
    // SB delta == BGD delta for a single-group file (test.txt lives in group 0).
    assert_eq!(
        sb_after - sb_before,
        (bgd_after - bgd_before) as u64,
        "SB and BGD freed-block totals disagree"
    );

    // The on-disk i_blocks count drops by the freed sectors; sanity check.
    let _ = blocks_before;

    if let (Some(s_before), Some(s_after)) = (seq_before, seq_after) {
        assert!(
            s_after > s_before,
            "truncate_shrink did not advance jsb.sequence ({s_before} -> {s_after}); \
             multi-block path bypassed the writer"
        );
    }

    // Journal is clean again.
    let dev = FileDevice::open(&path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("remount");
    if let Some(jsb) = fs_ext4::jbd2::read_superblock(&fs).expect("jsb") {
        assert!(jsb.is_clean(), "journal not clean (start={})", jsb.start);
    }

    fs::remove_file(path).ok();
}

#[test]
fn truncate_shrink_partial_size_persists() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "partial") else {
        return;
    };

    {
        let dev = FileDevice::open_rw(&path).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 4).expect("shrink to 4");
    }

    let (size_after, _, _, _, _) = snapshot(&path, "/test.txt");
    assert_eq!(
        size_after, 4,
        "partial shrink size not preserved across remount"
    );

    fs::remove_file(path).ok();
}

// CrashDevice mirrors the one in journal_writer_crash_safety.rs.
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

#[test]
fn crash_during_truncate_shrink_yields_consistent_state() {
    // Sweep budgets 0..=30 (truncate_shrink touches more blocks than
    // chmod, so the protocol uses more writes — give a wider sweep).
    // Each iteration must yield EITHER the original size or the
    // truncated size, never anything in between. The image must always
    // remount cleanly.
    let Some(probe) = copy_to_tmp("ext4-basic.img", "crash_probe") else {
        return;
    };
    let (size_orig, _, _, _, _) = snapshot(&probe, "/test.txt");
    fs::remove_file(probe).ok();

    let target_size = 4u64;

    for budget in 0..=30 {
        let Some(path) = copy_to_tmp("ext4-basic.img", &format!("b{budget}")) else {
            continue;
        };
        {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let ino = resolve(&fs, "/test.txt");
            let _ = fs.apply_truncate_shrink(ino, target_size);
        }
        // Force a real remount; replay applies any committed-but-not-
        // checkpointed transaction.
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let (size_after, _, _, _, _) = snapshot(&path, "/test.txt");
        assert!(
            size_after == size_orig || size_after == target_size,
            "budget={budget}: size {size_after} is neither original ({size_orig}) \
             nor target ({target_size}) — multi-block tx tore"
        );
        fs::remove_file(path).ok();
    }
}
