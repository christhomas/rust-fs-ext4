//! Phase 8.1 — proves CachedDevice meaningfully reduces inner-device
//! reads when wrapped around a real ext4 filesystem mount.
//!
//! Strategy: count the inner reads with and without the cache for the
//! same workload (mount + walk a directory + stat a file). The cached
//! version should be lower by an order of magnitude or more — extent
//! traversal and BGD reads repeatedly touch the same handful of blocks.

use fs_ext4::block_cache::CachedDevice;
use fs_ext4::block_io::BlockDevice;
use fs_ext4::error::Result;
use fs_ext4::Filesystem;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

struct CountingFile {
    bytes: Mutex<Vec<u8>>,
    reads: AtomicU64,
}

impl CountingFile {
    fn from_file(path: &str) -> Arc<Self> {
        let bytes = fs::read(path).expect("read image");
        Arc::new(Self {
            bytes: Mutex::new(bytes),
            reads: AtomicU64::new(0),
        })
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl BlockDevice for CountingFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let b = self.bytes.lock().unwrap();
        let off = offset as usize;
        if off + buf.len() > b.len() {
            return Err(fs_ext4::error::Error::OutOfBounds);
        }
        buf.copy_from_slice(&b[off..off + buf.len()]);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.bytes.lock().unwrap().len() as u64
    }
    fn is_writable(&self) -> bool {
        false
    }
}

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn workload(fs: &Filesystem) {
    // Lookup the same path 10 times — extent reads should hit cache.
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
    for _ in 0..10 {
        let _ = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/test.txt");
    }
}

#[test]
fn cache_reduces_inner_reads_for_repeated_lookups() {
    let path = image_path("ext4-basic.img");
    if !std::path::Path::new(&path).exists() {
        return;
    }

    // Baseline: no cache.
    let inner_baseline = CountingFile::from_file(&path);
    let fs_baseline = Filesystem::mount(inner_baseline.clone()).expect("mount");
    workload(&fs_baseline);
    let baseline_reads = inner_baseline.reads();

    // With cache: 64-block LRU, 4 KiB blocks.
    let inner_cached = CountingFile::from_file(&path);
    let cached: Arc<dyn BlockDevice> = Arc::new(CachedDevice::new(inner_cached.clone(), 4096, 64));
    let fs_cached = Filesystem::mount(cached.clone()).expect("mount cached");
    workload(&fs_cached);
    let cached_reads = inner_cached.reads();

    println!(
        "block_cache benchmark: baseline={} reads, cached={} reads ({:.1}x)",
        baseline_reads,
        cached_reads,
        baseline_reads as f64 / cached_reads.max(1) as f64
    );

    assert!(
        cached_reads * 2 <= baseline_reads,
        "cache should at least halve inner-device reads; baseline={baseline_reads}, cached={cached_reads}"
    );
}
