//! Negative tests for malformed / truncated / garbage images.
//!
//! The goal is not to exhaustively fuzz — cargo-fuzz belongs in a
//! separate harness — but to lock in the invariant that
//! `Filesystem::mount`, `read_inode_raw`, and basic dir walks
//! return `Err(...)` rather than panicking on untrusted input.
//!
//! Each test builds a synthetic `BlockDevice` whose bytes are
//! deliberately malformed in one way, and asserts we get a
//! structured error back instead of a panic.

use fs_ext4::block_io::BlockDevice;
use fs_ext4::error::{Error, Result};
use fs_ext4::fs::Filesystem;
use std::sync::Arc;

/// In-memory block device backed by a single `Vec<u8>`. Reads past EOF
/// fail, matching a real disk.
struct MemDevice {
    bytes: Vec<u8>,
}

impl MemDevice {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self { bytes })
    }
}

impl BlockDevice for MemDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let end = (offset as usize)
            .checked_add(buf.len())
            .ok_or(Error::OutOfBounds)?;
        if end > self.bytes.len() {
            return Err(Error::OutOfBounds);
        }
        buf.copy_from_slice(&self.bytes[offset as usize..end]);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }
}

/// Truncated image that doesn't even contain a full superblock block
/// must fail to mount, not panic.
#[test]
fn truncated_image_below_superblock_rejected() {
    let dev = MemDevice::new(vec![0u8; 512]);
    match Filesystem::mount(dev) {
        Ok(_) => panic!("unexpected Ok for 512-byte image"),
        Err(Error::Io(_))
        | Err(Error::BadMagic { .. })
        | Err(Error::Corrupt(_))
        | Err(Error::OutOfBounds) => {}
        Err(other) => panic!("unexpected error variant: {other:?}"),
    }
}

/// Image sized to hold a superblock but filled with zeros has no magic
/// and must be rejected with `BadMagic`.
#[test]
fn zero_filled_image_is_bad_magic() {
    // 2 MiB of zeros is enough for the superblock read to succeed.
    let dev = MemDevice::new(vec![0u8; 2 * 1024 * 1024]);
    match Filesystem::mount(dev) {
        Ok(_) => panic!("unexpected Ok for zero-filled image"),
        Err(Error::BadMagic { .. }) | Err(Error::Corrupt(_)) => {}
        Err(other) => panic!("expected BadMagic/Corrupt, got: {other:?}"),
    }
}

/// An image full of `0xFF` has a bogus magic and bogus feature flags.
/// Mounting must not panic — the test fails if the driver unwraps on
/// any of the garbage fields.
#[test]
fn all_ones_image_is_rejected_without_panic() {
    let dev = MemDevice::new(vec![0xFFu8; 2 * 1024 * 1024]);
    match Filesystem::mount(dev) {
        Ok(_) => panic!("unexpected Ok for all-ones image"),
        Err(Error::BadMagic { .. })
        | Err(Error::Corrupt(_))
        | Err(Error::UnsupportedIncompat(_))
        | Err(Error::UnsupportedRoCompat(_))
        | Err(Error::BadChecksum { .. })
        | Err(Error::Io(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

/// Deterministic PRNG byte-flood — sanity check that mount never panics
/// across a handful of seeds. Keeps running time small (8 × 64 KiB).
#[test]
fn prng_images_never_panic() {
    for seed in 0u64..8 {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xDEAD_BEEF_DEAD_BEEF;
        let mut bytes = vec![0u8; 64 * 1024];
        for b in bytes.iter_mut() {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            *b = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
        let dev = MemDevice::new(bytes);
        // Either Ok (astronomically unlikely but not illegal) or a
        // structured Err is fine; a panic would fail the whole test.
        let _ = Filesystem::mount(dev);
    }
}

/// Flip a single byte in a real ext4 image and confirm mount either
/// rejects it or reads without panicking.
#[test]
fn single_byte_flips_in_basic_image_dont_panic() {
    let bytes = match std::fs::read("test-disks/ext4-basic.img") {
        Ok(b) => b,
        Err(_) => {
            eprintln!("skip: test-disks/ext4-basic.img absent");
            return;
        }
    };
    // Flip bytes at a sampling of positions that are known to sit inside
    // a superblock, a block-group descriptor, and an inode table block.
    // (The exact offsets don't matter — any of these should either produce
    // a structured error or parse as benign noise; no panics allowed.)
    for &off in &[0x400u64, 0x404, 0x450, 0x500, 0x800, 0x1000, 0x4000, 0x8000] {
        if (off as usize) >= bytes.len() {
            continue;
        }
        let mut mutated = bytes.clone();
        mutated[off as usize] ^= 0xFF;
        let dev = MemDevice::new(mutated);
        let _ = Filesystem::mount(dev); // ok or err — just never panic
    }
}

/// Walk key on-disk structures of a byte-flipped ext4-basic image:
/// mount + root-inode read + root-extent-walk + reading the first
/// directory block. Every step must either return a structured Err
/// or succeed — NEVER panic. Any `.unwrap()` tripped by malformed
/// on-disk structures (dir entries, extent tree nodes, inode
/// fields, …) will light this up.
#[test]
fn dir_structure_walks_never_panic_on_byte_flipped_basic() {
    use fs_ext4::dir;
    use fs_ext4::extent;
    use fs_ext4::inode::Inode;

    let bytes = match std::fs::read("test-disks/ext4-basic.img") {
        Ok(b) => b,
        Err(_) => {
            eprintln!("skip: test-disks/ext4-basic.img absent");
            return;
        }
    };
    let sb_size = bytes.len();
    let sample_offsets: Vec<u64> = vec![
        0x400, 0x408, 0x450, 0x500, // superblock
        0x800, 0x820, 0x840, // bgdt
        0x2000, 0x2100, 0x2200, // inode table area
        0x4000, 0x4100, 0x5000, // root dir data block(s)
        0x8000, 0x10000,
    ];

    for &off in &sample_offsets {
        if off as usize >= sb_size {
            continue;
        }
        let mut mutated = bytes.clone();
        mutated[off as usize] ^= 0xFF;
        let dev = MemDevice::new(mutated);
        let Ok(fs) = Filesystem::mount(dev) else {
            continue;
        };

        // Root inode (ino 2).
        let Ok((inode, _raw)) = fs.read_inode_verified(2) else {
            continue;
        };
        // Extent walk over the root inode's extent tree.
        let _ = extent::collect_all(&inode.block, fs.dev.as_ref(), fs.sb.block_size());
        // Raw read of logical block 0 and parse as a directory block.
        if let Ok(Some(phys)) =
            extent::map_logical(&inode.block, fs.dev.as_ref(), fs.sb.block_size(), 0)
        {
            let mut block = vec![0u8; fs.sb.block_size() as usize];
            if fs
                .dev
                .read_at(phys * fs.sb.block_size() as u64, &mut block)
                .is_ok()
            {
                let _ = dir::parse_block(&block, true);
            }
        }
        // Inode 2 again but via the raw path.
        let _ = fs.read_inode_raw(2).and_then(|r| Inode::parse(&r));
    }
}

/// Build a minimal "superblock" buffer with valid magic but wildly
/// inconsistent inode parameters. Mount should surface a structured
/// error, never panic (pre-fix this tripped a div-by-zero).
#[test]
fn superblock_with_zero_inode_size_rejected() {
    // Start from a zero-filled 2 MiB image.
    let mut bytes = vec![0u8; 2 * 1024 * 1024];
    // ext4 magic at superblock offset 0x38 (from start of sb = 1024):
    bytes[1024 + 0x38] = 0x53;
    bytes[1024 + 0x39] = 0xEF;
    // Everything else is 0 — block_size=1024 (default for log=0), inode_size=0.
    // Expect a structured error (BadChecksum, Corrupt, UnsupportedFeature, etc.)
    let dev = MemDevice::new(bytes);
    if Filesystem::mount(dev).is_ok() {
        panic!("zero-inode-size must be rejected");
    }
    // Any structured Err is fine as long as we didn't panic.
}

/// Feed random bytes directly into `dir::parse_block`. The parser must
/// never panic — it should always either produce entries or return
/// `Err(CorruptDirEntry)`.
#[test]
fn dir_parse_block_never_panics_on_random_bytes() {
    use fs_ext4::dir;
    let mut state: u64 = 0xC0FF_EE00_DEAD_BEEF;
    for _ in 0..64 {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let len = 32 + (state as usize & 0x3FF);
        let mut buf = vec![0u8; len];
        for b in buf.iter_mut() {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            *b = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
        let _ = dir::parse_block(&buf, true);
        let _ = dir::parse_block(&buf, false);
        let _ = dir::has_csum_tail(&buf);
    }
}

/// Exercise the extent-header parser and leaf-extent parser on random bytes.
#[test]
fn extent_parsers_never_panic_on_random_bytes() {
    use fs_ext4::extent::{Extent, ExtentHeader, ExtentIdx};
    let mut state: u64 = 0xA5A5_A5A5_5A5A_5A5A;
    for _ in 0..128 {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let len = 12 + (state as usize & 0x7F);
        let mut buf = vec![0u8; len];
        for b in buf.iter_mut() {
            state ^= state >> 13;
            state ^= state << 7;
            *b = state as u8;
        }
        let _ = ExtentHeader::parse(&buf);
        let _ = Extent::parse(&buf);
        let _ = ExtentIdx::parse(&buf);
    }
}

/// Flip every single bit in the first 512 bytes of a real ext4 image
/// and confirm mounting never panics. This is the densest single-bit
/// flip surface available for the superblock region.
#[test]
fn exhaustive_single_bit_flip_first_sector_never_panics() {
    let bytes = match std::fs::read("test-disks/ext4-basic.img") {
        Ok(b) => b,
        Err(_) => {
            eprintln!("skip: test-disks/ext4-basic.img absent");
            return;
        }
    };
    for byte_off in 0x400..0x600usize {
        if byte_off >= bytes.len() {
            break;
        }
        for bit in 0..8u8 {
            let mut mutated = bytes.clone();
            mutated[byte_off] ^= 1 << bit;
            let dev = MemDevice::new(mutated);
            // Mount is allowed to succeed or fail; never panic.
            if let Ok(fs) = Filesystem::mount(dev) {
                let _ = fs.read_inode_verified(2);
            }
        }
    }
}
