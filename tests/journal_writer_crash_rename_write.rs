//! Phase 5.2.10 + 5.2.15 crash-safety sweeps for rename and
//! replace_file_content. Both ops touch many blocks (5+ for rename
//! cross-parent, more for big writes), so the budget sweep covers a
//! wide range. Atomicity contract: post-remount state must be either
//! pre-op or post-op; the image must always remount cleanly.

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
    let dst = format!("/tmp/fs_ext4_jw_crw_{}_{tag}_{n}.img", std::process::id());
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

fn exists(fs_path: &str, target: &str) -> bool {
    let dev = FileDevice::open(fs_path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, target).is_ok()
}

fn read_file_size(fs_path: &str, target: &str) -> Option<u64> {
    let dev = FileDevice::open(fs_path).expect("ro");
    let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, target).ok()?;
    Some(fs.read_inode_verified(ino).ok()?.0.size)
}

#[test]
fn crash_during_rename_yields_consistent_state() {
    // The fixture has /test.txt. We try rename to /renamed.txt under
    // each budget. Atomicity: at every interruption point the image
    // must mount cleanly AND BOTH (/test.txt exists, /renamed.txt
    // does not) OR (/test.txt gone, /renamed.txt exists). Anything
    // else is a tear.
    for budget in 0..=40 {
        let Some(path) = copy_to_tmp("ext4-basic.img", &format!("rename_b{budget}")) else {
            continue;
        };
        assert!(exists(&path, "/test.txt"), "fixture sanity");
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let _ = fs.apply_rename("/test.txt", "/renamed.txt");
        });
        assert!(result.is_ok(), "[budget={budget}] rename panicked");
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let src_exists = exists(&path, "/test.txt");
        let dst_exists = exists(&path, "/renamed.txt");
        // Valid post-states: pre-op (src yes, dst no) OR post-op
        // (src no, dst yes) OR transient (both visible briefly between
        // the buffered insert and the buffered remove — but since
        // BOTH are in the same BlockBuffer commit, this can only be
        // observed if the journal replay applied only part of the
        // transaction, which the four-fence protocol forbids).
        let valid = (src_exists && !dst_exists) || (!src_exists && dst_exists);
        assert!(
            valid,
            "[budget={budget}] rename torn: src_exists={src_exists}, dst_exists={dst_exists}"
        );
        fs::remove_file(path).ok();
    }
}

#[test]
fn crash_during_replace_file_content_yields_consistent_state() {
    // Replace the file with new content. After remount: i_size must
    // equal either the original size or the new payload's size — never
    // any other value (which would indicate a partially-applied tx).
    let Some(probe) = copy_to_tmp("ext4-basic.img", "rfc_probe") else {
        return;
    };
    let original_size = read_file_size(&probe, "/test.txt").expect("probe read");
    fs::remove_file(probe).ok();

    let payload = b"replaced content via journaled multi-block tx";
    let new_size = payload.len() as u64;

    for budget in 0..=40 {
        let Some(path) = copy_to_tmp("ext4-basic.img", &format!("rfc_b{budget}")) else {
            continue;
        };
        let result = std::panic::catch_unwind(|| {
            let inner = FileDevice::open_rw(&path).expect("rw");
            let crash = Arc::new(CrashDevice::new(Arc::new(inner), budget));
            let fs = Filesystem::mount(crash).expect("mount");
            let _ = fs.apply_replace_file_content("/test.txt", payload);
        });
        assert!(result.is_ok(), "[budget={budget}] replace panicked");
        let dev = FileDevice::open_rw(&path).expect("rw remount");
        let _ = Filesystem::mount(Arc::new(dev)).expect("remount");
        let post_size = read_file_size(&path, "/test.txt");
        match post_size {
            Some(s) if s == original_size || s == new_size => {}
            other => panic!(
                "[budget={budget}] replace torn: size {other:?} is neither \
                 original ({original_size}) nor new ({new_size})"
            ),
        }
        fs::remove_file(path).ok();
    }
}
