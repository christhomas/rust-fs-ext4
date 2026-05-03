//! Phase 8.1 — LRU block cache.
//!
//! `CachedDevice` wraps another `BlockDevice` with a small LRU keyed on
//! the device's logical block number. Read-heavy workloads (extent tree
//! traversal, repeated bitmap reads, directory walks) often hit the same
//! few blocks dozens of times per op; caching at the BlockDevice layer
//! amortizes that without touching higher-level code.
//!
//! Design choices:
//! - **Opt-in** via `CachedDevice::new(inner, block_size, capacity)`.
//!   Existing callers that go through `FileDevice` directly are
//!   unaffected — back-compat preserved.
//! - **Block-aligned reads only.** Multi-block reads (rare in this
//!   driver — most reads are exactly one fs block) bypass the cache and
//!   pass through to the inner device.
//! - **Write-through invalidation.** Any write covering a cached block
//!   evicts that block before delegating to the inner device. We don't
//!   write-back; the caller's flush still goes to inner.
//! - **Crash safety:** because we evict-on-write rather than buffer
//!   writes, the on-disk state is identical to what the inner device
//!   would produce. Crash semantics are unchanged.
//! - **No external LRU crate** — hand-rolled to avoid pulling in
//!   GPL/LGPL deps and to keep the cache logic auditable.

use crate::block_io::BlockDevice;
use crate::error::Result;
use std::collections::HashMap;
use std::sync::Mutex;

/// Inner LRU state. Held under a Mutex on `CachedDevice`. `lru_seq`
/// monotonically increases on every access; eviction picks the entry
/// with the lowest seq.
struct CacheState {
    capacity: usize,
    /// Block number → (bytes, last-access seq).
    entries: HashMap<u64, (Vec<u8>, u64)>,
    next_seq: u64,
    /// Hit / miss counters. Useful for benchmarks and the smoke test.
    hits: u64,
    misses: u64,
}

impl CacheState {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::with_capacity(capacity.min(1024)),
            next_seq: 0,
            hits: 0,
            misses: 0,
        }
    }

    fn next_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq = s.wrapping_add(1);
        s
    }

    /// Look up `block`. On hit: returns the cached bytes (clone) AND
    /// bumps the entry's recency. On miss: returns None.
    fn get(&mut self, block: u64) -> Option<Vec<u8>> {
        let seq = self.next_seq();
        if let Some(slot) = self.entries.get_mut(&block) {
            slot.1 = seq;
            self.hits += 1;
            Some(slot.0.clone())
        } else {
            self.misses += 1;
            None
        }
    }

    /// Insert `block` with `bytes`. Evicts the LRU entry first if at
    /// capacity. O(n) eviction since n is tiny by design (default 64).
    fn put(&mut self, block: u64, bytes: Vec<u8>) {
        if self.entries.len() >= self.capacity {
            // Evict the entry with the oldest seq.
            if let Some((&victim, _)) = self.entries.iter().min_by_key(|(_, (_, seq))| *seq) {
                self.entries.remove(&victim);
            }
        }
        let seq = self.next_seq();
        self.entries.insert(block, (bytes, seq));
    }

    /// Drop `block` from the cache (called on writes to invalidate).
    fn invalidate(&mut self, block: u64) {
        self.entries.remove(&block);
    }
}

/// LRU-cached BlockDevice. Pass-through for is_writable + size_bytes;
/// caches block-aligned reads, invalidates on writes.
pub struct CachedDevice {
    inner: std::sync::Arc<dyn BlockDevice>,
    block_size: u32,
    state: Mutex<CacheState>,
}

impl CachedDevice {
    /// Wrap `inner` with an LRU of `capacity` blocks. Pick `capacity`
    /// based on workload: 64 blocks (256 KiB at 4 KiB) is a reasonable
    /// default for general use; bigger directory walks benefit from 256+.
    pub fn new(inner: std::sync::Arc<dyn BlockDevice>, block_size: u32, capacity: usize) -> Self {
        Self {
            inner,
            block_size,
            state: Mutex::new(CacheState::new(capacity.max(1))),
        }
    }

    /// Snapshot (hits, misses) — useful for benchmarks and tests.
    pub fn stats(&self) -> (u64, u64) {
        let s = self.state.lock().expect("cache mutex poisoned");
        (s.hits, s.misses)
    }
}

impl BlockDevice for CachedDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let bs = self.block_size as u64;
        let block = offset / bs;
        let off_in_block = (offset % bs) as usize;
        let len = buf.len();

        // Block-aligned single-block read: cache fast path.
        if off_in_block + len <= bs as usize {
            // Try the cache first.
            {
                let mut state = self.state.lock().expect("cache mutex poisoned");
                if let Some(blk) = state.get(block) {
                    buf.copy_from_slice(&blk[off_in_block..off_in_block + len]);
                    return Ok(());
                }
            }
            // Miss: read the whole block from the inner device, then cache.
            let mut blk = vec![0u8; bs as usize];
            self.inner.read_at(block * bs, &mut blk)?;
            buf.copy_from_slice(&blk[off_in_block..off_in_block + len]);
            let mut state = self.state.lock().expect("cache mutex poisoned");
            state.put(block, blk);
            return Ok(());
        }

        // Multi-block read (rare): bypass the cache, pass through.
        self.inner.read_at(offset, buf)
    }

    fn size_bytes(&self) -> u64 {
        self.inner.size_bytes()
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        let bs = self.block_size as u64;
        let first_block = offset / bs;
        let last_block = (offset + buf.len() as u64).saturating_sub(1) / bs;
        // Invalidate before writing to avoid serving a stale read between
        // here and the inner write completing.
        {
            let mut state = self.state.lock().expect("cache mutex poisoned");
            for b in first_block..=last_block {
                state.invalidate(b);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Counts every read/write so we can prove the cache eliminates them.
    struct CountingDevice {
        bytes: Mutex<Vec<u8>>,
        reads: std::sync::atomic::AtomicU64,
        writes: std::sync::atomic::AtomicU64,
        writable: bool,
    }

    impl CountingDevice {
        fn new(size: usize, writable: bool) -> Arc<Self> {
            Arc::new(Self {
                bytes: Mutex::new(vec![0u8; size]),
                reads: std::sync::atomic::AtomicU64::new(0),
                writes: std::sync::atomic::AtomicU64::new(0),
                writable,
            })
        }
        fn reads(&self) -> u64 {
            self.reads.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn writes(&self) -> u64 {
            self.writes.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl BlockDevice for CountingDevice {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let b = self.bytes.lock().unwrap();
            let off = offset as usize;
            buf.copy_from_slice(&b[off..off + buf.len()]);
            Ok(())
        }
        fn size_bytes(&self) -> u64 {
            self.bytes.lock().unwrap().len() as u64
        }
        fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut b = self.bytes.lock().unwrap();
            let off = offset as usize;
            b[off..off + buf.len()].copy_from_slice(buf);
            Ok(())
        }
        fn is_writable(&self) -> bool {
            self.writable
        }
    }

    #[test]
    fn second_read_of_same_block_is_a_cache_hit() {
        let inner = CountingDevice::new(4096 * 16, false);
        let cached = CachedDevice::new(inner.clone(), 4096, 8);
        let mut buf = vec![0u8; 100];
        cached.read_at(0, &mut buf).unwrap();
        cached.read_at(0, &mut buf).unwrap();
        cached.read_at(50, &mut buf).unwrap(); // still in same block 0
        assert_eq!(inner.reads(), 1, "cache should serve all 3 from one read");
        let (hits, misses) = cached.stats();
        assert_eq!(hits, 2);
        assert_eq!(misses, 1);
    }

    #[test]
    fn write_invalidates_cache_entry() {
        let inner = CountingDevice::new(4096 * 16, true);
        let cached = CachedDevice::new(inner.clone(), 4096, 8);
        let mut buf = vec![0u8; 100];
        cached.read_at(0, &mut buf).unwrap();
        cached.write_at(0, &[42u8; 100]).unwrap();
        cached.read_at(0, &mut buf).unwrap();
        // After write the cache entry is gone — second read goes to disk.
        assert_eq!(inner.reads(), 2);
        assert_eq!(buf[0], 42, "post-write read should see the new bytes");
    }

    #[test]
    fn multi_block_read_bypasses_cache() {
        let inner = CountingDevice::new(4096 * 16, false);
        let cached = CachedDevice::new(inner.clone(), 4096, 8);
        let mut buf = vec![0u8; 8000]; // spans blocks 0 + 1
        cached.read_at(0, &mut buf).unwrap();
        // Cache wasn't populated → second multi-block read goes to disk again.
        cached.read_at(0, &mut buf).unwrap();
        assert_eq!(inner.reads(), 2);
        let (hits, misses) = cached.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0, "multi-block reads bypass entirely");
    }

    #[test]
    fn lru_evicts_oldest_when_capacity_exceeded() {
        let inner = CountingDevice::new(4096 * 16, false);
        let cached = CachedDevice::new(inner.clone(), 4096, 2); // capacity=2
        let mut buf = vec![0u8; 8];
        // Read blocks 0, 1, 2 — block 0 should be evicted.
        for blk in 0..3u64 {
            cached.read_at(blk * 4096, &mut buf).unwrap();
        }
        // Re-read block 0 → should miss (evicted).
        cached.read_at(0, &mut buf).unwrap();
        // Re-read block 2 → should hit (most recent).
        cached.read_at(2 * 4096, &mut buf).unwrap();
        let (hits, misses) = cached.stats();
        assert_eq!(misses, 4, "blocks 0,1,2 + re-read of 0 (evicted)");
        assert_eq!(hits, 1, "re-read of 2 still cached");
    }

    #[test]
    fn lru_keeps_recently_touched_block_alive() {
        // With capacity=2, reading [0, 1, 0, 2] keeps block 0 alive
        // because it was touched between 1 and 2 — block 1 should be
        // the eviction victim, not 0.
        let inner = CountingDevice::new(4096 * 16, false);
        let cached = CachedDevice::new(inner.clone(), 4096, 2);
        let mut buf = vec![0u8; 8];
        cached.read_at(0, &mut buf).unwrap(); // miss → cache
        cached.read_at(4096, &mut buf).unwrap(); // miss → cache
        cached.read_at(0, &mut buf).unwrap(); // hit, bumps recency
        cached.read_at(2 * 4096, &mut buf).unwrap(); // miss → evicts block 1
        cached.read_at(0, &mut buf).unwrap(); // hit (still cached)
        let (hits, misses) = cached.stats();
        assert_eq!(hits, 2);
        assert_eq!(misses, 3);
        // Verify block 1 was the eviction victim, not 0.
        cached.read_at(4096, &mut buf).unwrap(); // should miss
        let (_, m_after) = cached.stats();
        assert_eq!(m_after, 4, "block 1 was evicted, re-read is a miss");
    }
}
