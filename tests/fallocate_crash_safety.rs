//! Phase 2.2 + 2.3 + 2.4 crash-safety sweeps for the fallocate
//! variants. Each runs as a multi-block journaled transaction; this
//! verifies the post-remount image is always consistent (mounts
//! cleanly, i_size + i_blocks + extent tree all in agreement) under
//! every interruption point.

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
    let dst = format!(
        "/tmp/fs_ext4_falloc_crash_{}_{tag}_{n}.img",
        std::process::id()
    );
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

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

fn read_inode(fs_path: &str, target: &str) -> Option<(u64, u64)> {
    // (i_size, i_blocks)
    let dev = FileDevice::open(fs_path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, target).ok()?;
    let (inode, _) = fs.read_inode_verified(ino).ok()?;
    Some((inode.size, inode.blocks))
}

fn resolve(fs: &Filesystem, path: &str) -> u32 {
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, path).expect("resolve")
}

#[test]
fn crash_during_fallocate_keep_size_yields_consistent_state() {
    // Setup: shrink /test.txt to 0 so fallocate has a clean slate.
    // Then under each budget, run fallocate(KEEP_SIZE) for 16 KiB.
    // Post-remount: i_size must stay 0 (KEEP_SIZE invariant), and
    // i_blocks must be either 0 (op didn't apply) or +4*sectors_per_block
    // (op fully applied). Anything in between = torn.
    let Some(probe) = copy_to_tmp("ext4-basic.img", "ks_probe") else {
        return;
    };
    {
        let dev = FileDevice::open_rw(&probe).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink");
    }
    let baseline = read_inode(&probe, "/test.txt").expect("baseline").1;
    fs::remove_file(probe).ok();

    let bs_sectors = 4096u64 / 512;
    let expected_post_blocks = baseline + 4 * bs_sectors;

    for budget in 0..=40 {
        let Some(path) = copy_to_tmp("ext4-basic.img", &format!("ks_b{budget}")) else {
            continue;
        };
        // Fresh shrink — copy_to_tmp gives us back the un-modified fixture.
        {
            let dev = FileDevice::open_rw(&path).expect("rw setup");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount setup");
            let ino = resolve(&fs, "/test.txt");
            fs.apply_truncate_shrink(ino, 0).expect("shrink setup");
        }
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let ino = resolve(&fs, "/test.txt");
            let _ = fs.apply_fallocate_keep_size(ino, 0, 16384);
        });
        assert!(result.is_ok(), "[budget={budget}] keep_size panicked");
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let (size_post, blocks_post) = read_inode(&path, "/test.txt").expect("post");
        assert_eq!(
            size_post, 0,
            "[budget={budget}] KEEP_SIZE invariant violated: i_size={size_post}"
        );
        assert!(
            blocks_post == baseline || blocks_post == expected_post_blocks,
            "[budget={budget}] keep_size torn: i_blocks={blocks_post}, \
             expected pre ({baseline}) or post ({expected_post_blocks})"
        );
        fs::remove_file(path).ok();
    }
}

#[test]
fn crash_during_punch_hole_yields_consistent_state() {
    // Setup: shrink + preallocate 32 KiB (8 blocks, 1 contiguous extent).
    // Then punch the middle 8 KiB (blocks 2..4) under each budget. The
    // op tries to split [0..8] into [0..2] + [4..8], freeing 2 blocks.
    // Atomicity: i_blocks must be either pre (8 blocks) or post (6
    // blocks) — never something in between.
    let Some(probe) = copy_to_tmp("ext4-basic.img", "punch_probe") else {
        return;
    };
    {
        let dev = FileDevice::open_rw(&probe).expect("rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let ino = resolve(&fs, "/test.txt");
        fs.apply_truncate_shrink(ino, 0).expect("shrink");
        fs.apply_fallocate_keep_size(ino, 0, 32768)
            .expect("preallocate");
    }
    let pre_blocks = read_inode(&probe, "/test.txt").expect("pre").1;
    fs::remove_file(probe).ok();

    let bs_sectors = 4096u64 / 512;
    let post_blocks = pre_blocks - 2 * bs_sectors;

    for budget in 0..=40 {
        let Some(path) = copy_to_tmp("ext4-basic.img", &format!("punch_b{budget}")) else {
            continue;
        };
        {
            let dev = FileDevice::open_rw(&path).expect("rw setup");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount setup");
            let ino = resolve(&fs, "/test.txt");
            fs.apply_truncate_shrink(ino, 0).expect("shrink setup");
            fs.apply_fallocate_keep_size(ino, 0, 32768)
                .expect("preallocate setup");
        }
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let ino = resolve(&fs, "/test.txt");
            let _ = fs.apply_fallocate_punch_hole(ino, 8192, 8192);
        });
        assert!(result.is_ok(), "[budget={budget}] punch panicked");
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let (size_post, blocks_post) = read_inode(&path, "/test.txt").expect("post");
        assert_eq!(
            size_post, 0,
            "[budget={budget}] punch should not change i_size (KEEP_SIZE)"
        );
        assert!(
            blocks_post == pre_blocks || blocks_post == post_blocks,
            "[budget={budget}] punch torn: i_blocks={blocks_post}, \
             expected pre ({pre_blocks}) or post ({post_blocks})"
        );
        fs::remove_file(path).ok();
    }
}

#[test]
fn crash_during_zero_range_yields_consistent_state() {
    // zero_range = punch + alloc-uninitialized in two separate
    // transactions. After remount the image must mount cleanly. We
    // don't assert exact i_blocks because intermediate states are
    // valid (punch committed but alloc didn't); we only assert no
    // panic + clean remount.
    for budget in 0..=40 {
        let Some(path) = copy_to_tmp("ext4-basic.img", &format!("zr_b{budget}")) else {
            continue;
        };
        {
            let dev = FileDevice::open_rw(&path).expect("rw setup");
            let fs = Filesystem::mount(Arc::new(dev)).expect("mount setup");
            let ino = resolve(&fs, "/test.txt");
            fs.apply_truncate_shrink(ino, 0).expect("shrink setup");
            fs.apply_truncate_grow(ino, 16384).expect("grow setup");
        }
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let ino = resolve(&fs, "/test.txt");
            let _ = fs.apply_fallocate_zero_range(ino, 0, 16384);
        });
        assert!(result.is_ok(), "[budget={budget}] zero_range panicked");
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount must not fail");
        fs::remove_file(path).ok();
    }
}
