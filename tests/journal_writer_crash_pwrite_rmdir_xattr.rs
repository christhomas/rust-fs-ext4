//! Crash-safety sweeps for the write paths added/changed in the
//! metadata_csum write-corruption fix series:
//!
//! - `apply_pwrite` (single transaction) — a data write; had NO crash coverage
//!   at all.
//! - large `apply_pwrite` — the >1-transaction chunking path; each chunk commits
//!   as its own transaction, so a crash leaves a POSIX-legal partial write.
//! - `apply_rmdir` — now zeroes the freed directory inode; the zeroing must be
//!   part of the same atomic transaction as the dir-entry removal and the
//!   parent link-count decrement.
//! - `apply_removexattr` — the "last entry frees the external xattr block" path,
//!   rerouted through the journaled BlockBuffer flow; freeing the block,
//!   clearing i_file_acl, dropping i_blocks and rechecksum must all land
//!   atomically.
//!
//! A `CrashDevice` drops every write past a budget, modelling power-loss with
//! no torn sectors. After the crash run we remount with a real device (which
//! replays the journal) and assert the on-disk state is a discrete pre-op or
//! post-op view — never a half-applied tear — and that the image always mounts.
//! All sweeps run on the metadata_csum + metadata_csum_seed fixture (the exact
//! feature set that originally corrupted the Pi SD card).

use fs_ext4::block_io::{BlockDevice, FileDevice};
use fs_ext4::error::Result;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const IMG: &str = "ext4-csum-seed.img";

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
    let dst = format!("/tmp/fs_ext4_jw_cnp_{}_{tag}_{n}.img", std::process::id());
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// Drops writes after `write_budget` is exhausted; reads always pass through.
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

/// (i_size, i_blocks, links_count, i_file_acl) for `target`, or None if it
/// doesn't resolve.
fn read_fields(fs_path: &str, target: &str) -> Option<(u64, u64, u16, u64)> {
    let dev = FileDevice::open(fs_path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, target).ok()?;
    let (inode, _) = fs.read_inode_verified(ino).ok()?;
    Some((inode.size, inode.blocks, inode.links_count, inode.file_acl))
}

fn exists(fs_path: &str, target: &str) -> bool {
    read_fields(fs_path, target).is_some()
}

/// Link count of the root inode (#2).
fn root_links(fs_path: &str) -> u16 {
    let dev = FileDevice::open(fs_path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    fs.read_inode_verified(2).expect("root inode").0.links_count
}

#[test]
fn crash_during_pwrite_yields_consistent_state() {
    // /pw is created cleanly (outside the crash window), then an 8 KiB write
    // runs under each budget. A single pwrite journals its data blocks plus
    // the inode/bitmap/BGD/SB in ONE transaction, so post-remount i_size must
    // be pre (0) or post (8192) — never torn (size without blocks, or vice
    // versa).
    const N: u64 = 8192;
    for budget in 0..=40 {
        let Some(path) = copy_to_tmp(IMG, &format!("pw_b{budget}")) else {
            continue;
        };
        {
            let dev = FileDevice::open_rw(&path).expect("rw setup");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount setup");
            fs.apply_create("/pw", 0o644).expect("create setup");
        }
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let _ = fs.apply_pwrite("/pw", 0, &vec![0xABu8; N as usize]);
        });
        assert!(result.is_ok(), "[budget={budget}] pwrite panicked");

        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let (size, blocks, _, _) = read_fields(&path, "/pw").expect("post inode");
        assert!(
            size == 0 || size == N,
            "[budget={budget}] pwrite torn i_size={size} (expected 0 or {N})"
        );
        if size == 0 {
            assert_eq!(
                blocks, 0,
                "[budget={budget}] i_size 0 but i_blocks={blocks}"
            );
        } else {
            assert!(blocks > 0, "[budget={budget}] i_size {N} but i_blocks=0");
        }
        fs::remove_file(path).ok();
    }
}

#[test]
fn crash_during_large_pwrite_yields_consistent_state() {
    // A 2 MiB write overflows one transaction's descriptor capacity, so
    // apply_pwrite splits it into block-aligned chunks that each commit as
    // their OWN transaction. A crash between chunks therefore leaves a PARTIAL
    // write (some chunks committed) — POSIX-legal, so the invariant is weaker
    // than single-transaction ops: we don't pin i_size to pre-or-post, only
    // that the image always remounts with a self-consistent inode (i_size in
    // [0, full]; blocks and size both zero or both non-zero — no torn extent
    // tree). Budgets are coarse and span several chunk boundaries.
    const FULL: u64 = 2 * 1024 * 1024;
    for budget in [0usize, 5, 20, 60, 150, 400, 1000, 2500, 6000] {
        let Some(path) = copy_to_tmp(IMG, &format!("bigpw_b{budget}")) else {
            continue;
        };
        {
            let dev = FileDevice::open_rw(&path).expect("rw setup");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount setup");
            fs.apply_create("/big", 0o644).expect("create setup");
        }
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let _ = fs.apply_pwrite("/big", 0, &vec![0xC3u8; FULL as usize]);
        });
        assert!(result.is_ok(), "[budget={budget}] large pwrite panicked");

        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount must succeed");
        let (size, blocks, _, _) = read_fields(&path, "/big").expect("post inode");
        assert!(
            size <= FULL,
            "[budget={budget}] i_size {size} exceeds full {FULL}"
        );
        if size == 0 {
            assert_eq!(
                blocks, 0,
                "[budget={budget}] i_size 0 but i_blocks={blocks}"
            );
        }
        if blocks == 0 {
            assert_eq!(size, 0, "[budget={budget}] i_blocks 0 but i_size={size}");
        }
        fs::remove_file(path).ok();
    }
}

#[test]
fn crash_during_rmdir_yields_consistent_state() {
    // mkdir /victim (clean) bumps the PARENT (root) link count by 1 for
    // victim's ".." backlink. rmdir must atomically remove the dir entry, free
    // + zero the dir inode, and decrement root's links. Post-remount, victim is
    // fully present (pre) or fully gone (post), and root's link count must
    // AGREE with victim's existence — the tear detector.
    for budget in 0..=40 {
        let Some(path) = copy_to_tmp(IMG, &format!("rmdir_b{budget}")) else {
            continue;
        };
        let links_before_mkdir;
        let links_with_victim;
        {
            let dev = FileDevice::open_rw(&path).expect("rw setup");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount setup");
            links_before_mkdir = fs.read_inode_verified(2).expect("root").0.links_count;
            fs.apply_mkdir("/victim", 0o755).expect("mkdir setup");
            links_with_victim = fs.read_inode_verified(2).expect("root").0.links_count;
        }
        assert_eq!(
            links_with_victim,
            links_before_mkdir + 1,
            "[budget={budget}] setup: mkdir should bump root links"
        );

        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let _ = fs.apply_rmdir("/victim");
        });
        assert!(result.is_ok(), "[budget={budget}] rmdir panicked");

        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let present = exists(&path, "/victim");
        let rl = root_links(&path);
        if present {
            assert_eq!(
                rl, links_with_victim,
                "[budget={budget}] /victim present but root links torn (got {rl})"
            );
        } else {
            assert_eq!(
                rl, links_before_mkdir,
                "[budget={budget}] /victim gone but root links not decremented (got {rl})"
            );
        }
        fs::remove_file(path).ok();
    }
}

#[test]
fn crash_during_removexattr_frees_block_yields_consistent_state() {
    // Setup: /xf with a large user.big xattr → spills to an EXTERNAL xattr
    // block, so i_file_acl points at it and i_blocks counts it. removexattr of
    // the only entry must atomically free that block, clear i_file_acl, drop
    // i_blocks and recompute the inode checksum. Post-remount the pointer and
    // the block count must agree: both still owned (pre) or both released
    // (post) — never a dangling pointer or a leaked/double-counted block.
    for budget in 0..=40 {
        let Some(path) = copy_to_tmp(IMG, &format!("rmxattr_b{budget}")) else {
            continue;
        };
        {
            let dev = FileDevice::open_rw(&path).expect("rw setup");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount setup");
            fs.apply_create("/xf", 0o644).expect("create setup");
            fs.apply_setxattr("/xf", "user.big", &vec![0x7Eu8; 3072])
                .expect("setxattr setup");
        }
        // Read the post-setup fields from a fresh mount so the external block
        // is visible on disk.
        let (_, blocks_pre, _, acl_pre) = read_fields(&path, "/xf").expect("setup fields");
        assert!(
            acl_pre != 0,
            "[budget={budget}] setup: expected an external xattr block (i_file_acl != 0)"
        );

        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let _ = fs.apply_removexattr("/xf", "user.big");
        });
        assert!(result.is_ok(), "[budget={budget}] removexattr panicked");

        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let (_, blocks_post, _, acl_post) = read_fields(&path, "/xf").expect("post inode");
        let pre = acl_post == acl_pre && blocks_post == blocks_pre;
        let post = acl_post == 0 && blocks_post < blocks_pre;
        assert!(
            pre || post,
            "[budget={budget}] removexattr torn: acl {acl_pre}->{acl_post}, \
             blocks {blocks_pre}->{blocks_post}"
        );
        fs::remove_file(path).ok();
    }
}
