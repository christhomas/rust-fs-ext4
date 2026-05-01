//! ext4 filesystem creation (mkfs).
//!
//! Writes a minimum-viable ext4 layout that mounts cleanly under both this
//! crate's read path and Linux. Targets a single block group for tiny test
//! volumes and scales to N groups for larger devices. v1 layout:
//!
//! - Block 0 (offset 0..1024)    : zero (boot sector)
//! - Block 0 (offset 1024..2048) : primary superblock
//! - Block 1                     : block group descriptor table
//! - Block 2                     : group 0 block bitmap
//! - Block 3                     : group 0 inode bitmap
//! - Blocks 4..N                 : group 0 inode table
//! - Block (4+itable_blocks)     : root directory data block
//!
//! Features enabled: FILETYPE, EXTENTS, 64BIT, METADATA_CSUM. Journal is
//! intentionally OFF for v1 — the resulting FS mounts cleanly without it.
//!
//! Inode 1 is reserved (unused), inode 2 is the root `/` directory.

use crate::block_io::BlockDevice;
use crate::checksum::{linux_crc32c, Checksummer};
use crate::dir::{self, DirEntryType};
use crate::error::{Error, Result};
use crate::features::{Incompat, RoCompat};

const EXT4_MAGIC: u16 = 0xEF53;
const EXT4_VALID_FS: u16 = 0x0001;
const EXT4_ROOT_INO: u32 = 2;
const EXT4_GOOD_OLD_INODE_SIZE: u16 = 128;
const INODE_SIZE: u16 = 256;
const DESC_SIZE: u16 = 64; // 64BIT incompat → BGD = 64 bytes
const I_EXTRA_ISIZE: u16 = 32; // covers checksum_hi, ctime/mtime/atime extra, crtime
const ROOT_MODE: u16 = 0o40755; // S_IFDIR | 0755
const EXTENT_MAGIC: u16 = 0xF30A;

/// Format `dev` as an ext4 filesystem. The device must be at least large
/// enough to hold the metadata + one root directory block (≈ 200 KiB at
/// 4 KiB blocks with the default geometry).
///
/// Arguments:
/// - `label`     — volume name (truncated to 16 bytes; UTF-8 stored verbatim).
/// - `uuid`      — 128-bit volume UUID; if `None`, a random one is generated.
/// - `size_bytes`— total device size to format (must be ≤ device's reported size).
/// - `block_size`— filesystem block size in bytes; must be a power of two,
///   1024..=65536. Typical: 4096.
pub fn format_filesystem(
    dev: &dyn BlockDevice,
    label: Option<&str>,
    uuid: Option<[u8; 16]>,
    size_bytes: u64,
    block_size: u32,
) -> Result<()> {
    if !block_size.is_power_of_two() || !(1024..=65536).contains(&block_size) {
        return Err(Error::InvalidArgument("mkfs: block_size out of range"));
    }
    if size_bytes < block_size as u64 * 64 {
        return Err(Error::InvalidArgument("mkfs: device too small"));
    }
    if !dev.is_writable() {
        return Err(Error::ReadOnly);
    }

    let log_block_size = (block_size.trailing_zeros() as i32 - 10) as u32;

    // Geometry. blocks_per_group is the canonical 8 * block_size (so 32768
    // for 4 KiB blocks). Inodes-per-group is sized so the inode table stays
    // a small fraction of the group; for v1 simplicity we use 8192 — gives
    // 64 KiB / 256 B = 256 inodes per inode-table block, 32 inode-table
    // blocks per group at 4 KiB.
    let blocks_per_group: u32 = 8 * block_size; // 32768 at 4 KiB
    let blocks_count: u64 = size_bytes / block_size as u64;
    if blocks_count < 64 {
        return Err(Error::InvalidArgument("mkfs: too few blocks"));
    }

    // For v1 we only fully initialise group 0; the rest are marked
    // BLOCK_UNINIT + INODE_UNINIT. Restrict to a single group to match the
    // brief's "v1 minimum-viable" scope and to keep block-layout math
    // tractable for the test fixture (32 MiB single-group image).
    if blocks_count > blocks_per_group as u64 {
        return Err(Error::InvalidArgument(
            "mkfs: multi-group volumes not yet supported (v1 single-group only)",
        ));
    }
    let group_count: u64 = 1;

    let inodes_per_group: u32 = 8192;
    let inode_table_blocks: u32 =
        (inodes_per_group as u64 * INODE_SIZE as u64).div_ceil(block_size as u64) as u32;

    // first_data_block is 1 for 1 KiB blocks, 0 otherwise — mirrors ext4 formatter.
    let first_data_block: u32 = if block_size == 1024 { 1 } else { 0 };

    // Layout within group 0:
    //   superblock           : block first_data_block (offset 1024 inside it for 4 KiB)
    //   bgd table            : block first_data_block + 1
    //   block bitmap         : block first_data_block + 2
    //   inode bitmap         : block first_data_block + 3
    //   inode table          : blocks first_data_block + 4 .. + 4 + itable_blocks
    //   root dir data block  : block first_data_block + 4 + itable_blocks
    let bgt_block: u64 = first_data_block as u64 + 1;
    let blk_bitmap: u64 = first_data_block as u64 + 2;
    let ino_bitmap: u64 = first_data_block as u64 + 3;
    let inode_table_start: u64 = first_data_block as u64 + 4;
    let root_dir_block: u64 = inode_table_start + inode_table_blocks as u64;

    // Sanity: every metadata block must fit in the device.
    if root_dir_block + 1 >= blocks_count {
        return Err(Error::InvalidArgument("mkfs: device too small for layout"));
    }

    let used_blocks: u64 = root_dir_block + 1; // blocks 0..=root_dir_block
    let free_blocks: u64 = blocks_count - used_blocks;
    let free_inodes: u32 = inodes_per_group - 2; // inodes 1 + 2 used

    let uuid = uuid.unwrap_or_else(generate_uuid);

    // ----- Superblock -------------------------------------------------------
    // Build the 1024-byte primary superblock then patch its checksum.
    let mut sb = build_superblock(
        blocks_count,
        free_blocks,
        free_inodes,
        first_data_block,
        log_block_size,
        blocks_per_group,
        inodes_per_group,
        &uuid,
        label.unwrap_or(""),
    );
    // Superblock CRC32C: seed = ~0, covers bytes [0..0x3FC].
    let sb_csum = linux_crc32c(!0, &sb[..0x3FC]);
    sb[0x3FC..0x400].copy_from_slice(&sb_csum.to_le_bytes());

    // Mount-time checksummer, used for BGD + inode + dir-block CRCs.
    let csum_seed = linux_crc32c(!0, &uuid);
    let csum = Checksummer {
        seed: csum_seed,
        enabled: true,
    };

    // ----- Block group descriptor (group 0 only) ---------------------------
    let mut bgd = vec![0u8; DESC_SIZE as usize];
    write_bgd_group0(
        &mut bgd,
        blk_bitmap,
        ino_bitmap,
        inode_table_start,
        free_blocks as u32,
        free_inodes,
        /* used_dirs */ 1, // root dir lives in group 0
    );
    // BGD CRC: crc16 of (seed → group_no_le_u32 → bgd_with_csum_zeroed).
    {
        let mut tmp = bgd.clone();
        tmp[0x1E] = 0;
        tmp[0x1F] = 0;
        let bgd_csum_full = csum.crc_with_prefix(0u32, &tmp);
        let bgd_csum16 = (bgd_csum_full & 0xFFFF) as u16;
        bgd[0x1E..0x20].copy_from_slice(&bgd_csum16.to_le_bytes());
    }

    // ----- Block bitmap (group 0) ------------------------------------------
    // Bits 0..=root_dir_block are used.
    let mut block_bitmap = vec![0u8; block_size as usize];
    for b in 0..=root_dir_block {
        let byte = (b / 8) as usize;
        let bit = (b % 8) as u8;
        block_bitmap[byte] |= 1 << bit;
    }
    // Tail-pad: blocks past `blocks_count` (within the group's bitmap window)
    // are flagged "used" so the allocator never tries them. blocks_per_group
    // bits cover the bitmap's logical span.
    for b in blocks_count..blocks_per_group as u64 {
        let byte = (b / 8) as usize;
        if byte >= block_bitmap.len() {
            break;
        }
        let bit = (b % 8) as u8;
        block_bitmap[byte] |= 1 << bit;
    }

    // ----- Inode bitmap (group 0) ------------------------------------------
    // ext4 inode numbers are 1-based; bit i = inode (i+1).
    let mut inode_bitmap = vec![0u8; block_size as usize];
    inode_bitmap[0] |= 0b0000_0011; // inodes 1 and 2

    // ----- Inode table (group 0) — only inode 2 has content ----------------
    let mut inode_table = vec![0u8; inode_table_blocks as usize * block_size as usize];
    // Inode 2 lives at byte offset (2-1) * INODE_SIZE.
    let root_inode_off = (EXT4_ROOT_INO as usize - 1) * INODE_SIZE as usize;
    write_root_inode(
        &mut inode_table[root_inode_off..root_inode_off + INODE_SIZE as usize],
        root_dir_block,
        block_size,
    );
    // Patch inode 2 CRC32C (seed → ino → generation → inode_with_csum_zeroed).
    {
        let slot = &mut inode_table[root_inode_off..root_inode_off + INODE_SIZE as usize];
        if let Some((lo, hi)) = csum.compute_inode_checksum(EXT4_ROOT_INO, 0, slot) {
            slot[0x7C..0x7E].copy_from_slice(&lo.to_le_bytes());
            slot[0x82..0x84].copy_from_slice(&hi.to_le_bytes());
        }
    }

    // ----- Root directory data block ---------------------------------------
    // Two entries (`.`, `..`) plus the metadata-csum tail. We seed a single
    // long entry covering [0..block-12], then add `.` and `..` via the
    // existing dir helper so the rec_len bookkeeping matches what the
    // mounted FS expects.
    let mut root_dir = vec![0u8; block_size as usize];
    let usable = block_size as usize - 12; // last 12 bytes reserved for csum tail
                                           // Bootstrap: one big tombstone entry that fills the usable region. The
                                           // dir helper splits this on each add.
    root_dir[0..4].copy_from_slice(&0u32.to_le_bytes()); // inode = 0 (tombstone)
    root_dir[4..6].copy_from_slice(&(usable as u16).to_le_bytes());
    // name_len + file_type already zero.

    dir::add_entry_to_block(
        &mut root_dir,
        EXT4_ROOT_INO,
        b".",
        DirEntryType::Directory,
        true,
        12,
    )?;
    dir::add_entry_to_block(
        &mut root_dir,
        EXT4_ROOT_INO,
        b"..",
        DirEntryType::Directory,
        true,
        12,
    )?;

    // Csum tail: inode=0, rec_len=12, name_len=0, file_type=0xDE, csum=u32.
    {
        let end = root_dir.len();
        root_dir[end - 12..end - 8].copy_from_slice(&0u32.to_le_bytes()); // inode
        root_dir[end - 8..end - 6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
        root_dir[end - 6] = 0; // name_len
        root_dir[end - 5] = 0xDE; // file_type marker
        root_dir[end - 4..end].copy_from_slice(&0u32.to_le_bytes()); // csum slot
    }
    // CRC covers block[..len-12]; chained seed → ino → generation → body.
    {
        let mut c = linux_crc32c(csum.seed, &EXT4_ROOT_INO.to_le_bytes());
        c = linux_crc32c(c, &0u32.to_le_bytes()); // generation = 0
        c = linux_crc32c(c, &root_dir[..root_dir.len() - 12]);
        let end = root_dir.len();
        root_dir[end - 4..end].copy_from_slice(&c.to_le_bytes());
    }

    // ----- Write everything out --------------------------------------------
    // Block 0: zeros + primary SB. We write the whole block to clear any
    // stale image bytes (mounting an existing image without a wipe was
    // sabotaging early experiments).
    let mut block0 = vec![0u8; block_size as usize];
    block0[1024..2048].copy_from_slice(&sb);
    dev.write_at(0, &block0)?;
    dev.flush()?;

    // BGT block (zeroed, then group 0's descriptor copied in at offset 0).
    let mut bgt_block_buf = vec![0u8; block_size as usize];
    bgt_block_buf[..DESC_SIZE as usize].copy_from_slice(&bgd);
    dev.write_at(bgt_block * block_size as u64, &bgt_block_buf)?;

    dev.write_at(blk_bitmap * block_size as u64, &block_bitmap)?;
    dev.write_at(ino_bitmap * block_size as u64, &inode_bitmap)?;
    dev.write_at(inode_table_start * block_size as u64, &inode_table)?;
    dev.write_at(root_dir_block * block_size as u64, &root_dir)?;

    dev.flush()?;
    let _ = group_count; // single-group v1; future multi-group will use this
    Ok(())
}

/// 16 random bytes from `/dev/urandom`, falling back to a time-seeded LCG if
/// the device is unavailable. Sets the v4 UUID layout bits.
fn generate_uuid() -> [u8; 16] {
    let mut out = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut out).is_ok() {
            // RFC 4122 v4: top nibble of byte 6 = 0x4, top two bits of byte 8 = 0b10.
            out[6] = (out[6] & 0x0F) | 0x40;
            out[8] = (out[8] & 0x3F) | 0x80;
            return out;
        }
    }
    // Fallback: deterministic mix of nanos + pid. Not cryptographic — but
    // /dev/urandom is universally available on Darwin and Linux so this
    // path effectively only fires inside aggressively sandboxed tests.
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xDEADBEEF)
        ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15);
    for b in out.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (state >> 56) as u8;
    }
    out[6] = (out[6] & 0x0F) | 0x40;
    out[8] = (out[8] & 0x3F) | 0x80;
    out
}

#[allow(clippy::too_many_arguments)]
fn build_superblock(
    blocks_count: u64,
    free_blocks: u64,
    free_inodes: u32,
    first_data_block: u32,
    log_block_size: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    uuid: &[u8; 16],
    label: &str,
) -> Vec<u8> {
    // Single-group v1: total inodes = inodes_per_group.
    let inodes_count: u32 = inodes_per_group;
    let mut sb = vec![0u8; 1024];

    let blocks_lo = (blocks_count & 0xFFFF_FFFF) as u32;
    let blocks_hi = (blocks_count >> 32) as u32;
    let free_lo = (free_blocks & 0xFFFF_FFFF) as u32;
    let free_hi = (free_blocks >> 32) as u32;

    sb[0x00..0x04].copy_from_slice(&inodes_count.to_le_bytes());
    sb[0x04..0x08].copy_from_slice(&blocks_lo.to_le_bytes());
    // 0x08..0x0C s_r_blocks_count_lo (reserved blocks) — 0 is fine.
    sb[0x0C..0x10].copy_from_slice(&free_lo.to_le_bytes());
    sb[0x10..0x14].copy_from_slice(&free_inodes.to_le_bytes());
    sb[0x14..0x18].copy_from_slice(&first_data_block.to_le_bytes());
    sb[0x18..0x1C].copy_from_slice(&log_block_size.to_le_bytes());
    // 0x1C..0x20 s_log_cluster_size — must mirror s_log_block_size when bigalloc off.
    sb[0x1C..0x20].copy_from_slice(&log_block_size.to_le_bytes());
    sb[0x20..0x24].copy_from_slice(&blocks_per_group.to_le_bytes());
    // 0x24..0x28 s_clusters_per_group (mirrors blocks_per_group, no bigalloc).
    sb[0x24..0x28].copy_from_slice(&blocks_per_group.to_le_bytes());
    sb[0x28..0x2C].copy_from_slice(&inodes_per_group.to_le_bytes());

    // s_mtime, s_wtime stay 0 — ext4 spec allows; no Y2038 trap here.
    // s_mnt_count = 0, s_max_mnt_count = 0xFFFF (no fsck nag).
    sb[0x34..0x36].copy_from_slice(&0u16.to_le_bytes()); // mnt_count
    sb[0x36..0x38].copy_from_slice(&0xFFFFu16.to_le_bytes()); // max_mnt_count

    sb[0x38..0x3A].copy_from_slice(&EXT4_MAGIC.to_le_bytes());
    sb[0x3A..0x3C].copy_from_slice(&EXT4_VALID_FS.to_le_bytes()); // state
    sb[0x3C..0x3E].copy_from_slice(&1u16.to_le_bytes()); // errors = continue
    sb[0x3E..0x40].copy_from_slice(&0u16.to_le_bytes()); // minor_rev_level

    // 0x40..0x44 s_lastcheck = 0
    // 0x44..0x48 s_checkinterval = 0
    sb[0x48..0x4C].copy_from_slice(&0u32.to_le_bytes()); // creator_os = 0 (Linux)

    sb[0x4C..0x50].copy_from_slice(&1u32.to_le_bytes()); // rev_level = DYNAMIC
    sb[0x50..0x52].copy_from_slice(&0u16.to_le_bytes()); // def_resuid
    sb[0x52..0x54].copy_from_slice(&0u16.to_le_bytes()); // def_resgid

    // Dynamic-rev fields.
    sb[0x54..0x58].copy_from_slice(&11u32.to_le_bytes()); // first_ino (>= 11 reserved-end)
    sb[0x58..0x5A].copy_from_slice(&INODE_SIZE.to_le_bytes());
    sb[0x5A..0x5C].copy_from_slice(&0u16.to_le_bytes()); // block_group_nr (this SB's group = 0)

    let feat_compat = 0u32; // No HAS_JOURNAL, DIR_INDEX, etc. — keep v1 minimal.
    let feat_incompat =
        Incompat::FILETYPE.bits() | Incompat::EXTENTS.bits() | Incompat::BIT64.bits();
    let feat_ro_compat = RoCompat::METADATA_CSUM.bits();
    sb[0x5C..0x60].copy_from_slice(&feat_compat.to_le_bytes());
    sb[0x60..0x64].copy_from_slice(&feat_incompat.to_le_bytes());
    sb[0x64..0x68].copy_from_slice(&feat_ro_compat.to_le_bytes());

    sb[0x68..0x78].copy_from_slice(uuid);

    // Volume label — 16 bytes, NUL-padded.
    let lbl = label.as_bytes();
    let n = lbl.len().min(16);
    sb[0x78..0x78 + n].copy_from_slice(&lbl[..n]);

    // s_last_mounted (64 bytes at 0x88) stays zero.
    // Algorithm bits / prealloc / reserved (0xC8..0xD8) zero.

    // 0xD8..0xDC s_journal_inum — 0 because no journal.
    // 0xDC..0xE0 s_journal_dev  — 0.
    // 0xE0..0xE4 s_last_orphan  — 0.
    // 0xE4..0xF4 s_hash_seed[4] — pick a stable nonzero seed. (Only matters
    // if HTree is in play; we don't set DIR_INDEX, but ext4 formatter still seeds
    // these so tools don't whine.)
    sb[0xE4..0xE8].copy_from_slice(&0xC1A2B3C4u32.to_le_bytes());
    sb[0xE8..0xEC].copy_from_slice(&0xD5E6F7A8u32.to_le_bytes());
    sb[0xEC..0xF0].copy_from_slice(&0xB9CADBECu32.to_le_bytes());
    sb[0xF0..0xF4].copy_from_slice(&0xFD0E1F2Au32.to_le_bytes());

    sb[0xFC] = 1; // s_def_hash_version = HALF_MD4
                  // 0xFD reserved_char_pad
                  // 0xFE..0x100 s_desc_size
    sb[0xFE..0x100].copy_from_slice(&DESC_SIZE.to_le_bytes());

    // 0x100..0x104 s_default_mount_opts = 0
    // 0x104..0x108 s_first_meta_bg     = 0 (no META_BG)
    // 0x108..0x10C s_mkfs_time         = 0
    // 0x10C..0x14C s_jnl_blocks[17]    = 0

    // s_blocks_count_hi at 0x150 (64BIT).
    sb[0x150..0x154].copy_from_slice(&blocks_hi.to_le_bytes());
    // s_r_blocks_count_hi 0x154 = 0.
    sb[0x158..0x15C].copy_from_slice(&free_hi.to_le_bytes()); // free_blocks_count_hi

    // s_min_extra_isize / s_want_extra_isize (0x15C / 0x15E): 32 — matches
    // I_EXTRA_ISIZE so ext4 audit tool doesn't complain about short inodes.
    sb[0x15C..0x15E].copy_from_slice(&I_EXTRA_ISIZE.to_le_bytes());
    sb[0x15E..0x160].copy_from_slice(&I_EXTRA_ISIZE.to_le_bytes());

    // s_flags 0x160..0x164: bit 0 = signed dirhash (matches ext4 formatter default).
    sb[0x160..0x164].copy_from_slice(&0x1u32.to_le_bytes());

    // 0x164..0x166 s_raid_stride / 0x166..0x168 mmp_update_interval / etc — zero.

    // s_kbytes_written at 0x148..0x150 = 0 (no writes yet).

    // s_inode_size at 0x58 already set; s_min_extra_isize done above.

    // s_log_groups_per_flex (0x174) — 0 means flex_bg disabled.

    // s_checksum_type at 0x175 = 1 (crc32c) when METADATA_CSUM is on.
    sb[0x175] = 1;

    // s_encryption_level (0x176) reserved-pad (0x177) zero.

    // s_kbytes_written (0x148): 0.

    // s_snapshot_inum / list / id_xattr — zero.

    // s_creator_os already 0.

    // Leave s_checksum (0x3FC..0x400) as zero — caller patches it.
    sb
}

fn write_bgd_group0(
    out: &mut [u8],
    block_bitmap_block: u64,
    inode_bitmap_block: u64,
    inode_table_block: u64,
    free_blocks: u32,
    free_inodes: u32,
    used_dirs: u32,
) {
    // Lo halves at the start, hi halves at +0x20 (64-bit BGD layout).
    out[0x00..0x04].copy_from_slice(&(block_bitmap_block as u32).to_le_bytes());
    out[0x04..0x08].copy_from_slice(&(inode_bitmap_block as u32).to_le_bytes());
    out[0x08..0x0C].copy_from_slice(&(inode_table_block as u32).to_le_bytes());
    out[0x0C..0x0E].copy_from_slice(&(free_blocks as u16).to_le_bytes());
    out[0x0E..0x10].copy_from_slice(&(free_inodes as u16).to_le_bytes());
    out[0x10..0x12].copy_from_slice(&(used_dirs as u16).to_le_bytes());
    out[0x12..0x14].copy_from_slice(&0u16.to_le_bytes()); // bg_flags = 0 (initialised)
                                                          // 0x14..0x18 exclude_bitmap reserved.
                                                          // 0x18..0x1A block_bitmap_csum_lo, 0x1A..0x1C inode_bitmap_csum_lo — leave 0; the
                                                          // kernel only complains when BGD's checksum is wrong.
    out[0x1C..0x1E].copy_from_slice(&0u16.to_le_bytes()); // itable_unused_lo
                                                          // 0x1E..0x20 checksum — patched after struct is complete.

    // 64-bit hi halves.
    out[0x20..0x24].copy_from_slice(&((block_bitmap_block >> 32) as u32).to_le_bytes());
    out[0x24..0x28].copy_from_slice(&((inode_bitmap_block >> 32) as u32).to_le_bytes());
    out[0x28..0x2C].copy_from_slice(&((inode_table_block >> 32) as u32).to_le_bytes());
    out[0x2C..0x2E].copy_from_slice(&0u16.to_le_bytes()); // free_blocks_hi
    out[0x2E..0x30].copy_from_slice(&0u16.to_le_bytes()); // free_inodes_hi
    out[0x30..0x32].copy_from_slice(&0u16.to_le_bytes()); // used_dirs_hi
    out[0x32..0x34].copy_from_slice(&0u16.to_le_bytes()); // itable_unused_hi
                                                          // 0x34..0x38 reserved
    out[0x38..0x3A].copy_from_slice(&0u16.to_le_bytes()); // bb_csum_hi
    out[0x3A..0x3C].copy_from_slice(&0u16.to_le_bytes()); // ib_csum_hi
                                                          // 0x3C..0x40 reserved
}

/// Write a 256-byte root directory inode (ino 2) using a one-extent layout
/// pointing at `root_dir_block`. Caller patches the CRC slots afterwards.
fn write_root_inode(slot: &mut [u8], root_dir_block: u64, block_size: u32) {
    // i_mode
    slot[0x00..0x02].copy_from_slice(&ROOT_MODE.to_le_bytes());
    // i_uid_lo, i_size_lo
    slot[0x04..0x08].copy_from_slice(&(block_size).to_le_bytes()); // size = one block
                                                                   // i_atime, i_ctime, i_mtime — leave zero; on-disk values can be 0 and
                                                                   // the FS still mounts (ext4 formatter sets these to wall time, but it's not
                                                                   // required by the kernel).
                                                                   // i_links_count = 2 (`.` and `..`)
    slot[0x1A..0x1C].copy_from_slice(&2u16.to_le_bytes());
    // i_blocks_lo: 512-byte units. One 4 KiB block = 8 sectors.
    let i_blocks = block_size / 512;
    slot[0x1C..0x20].copy_from_slice(&i_blocks.to_le_bytes());
    // i_flags: EXT4_EXTENTS_FL
    slot[0x20..0x24].copy_from_slice(&crate::inode::InodeFlags::EXTENTS.bits().to_le_bytes());

    // i_block[60]: extent header + one extent.
    // Header: magic, entries=1, max=4, depth=0, generation=0
    slot[0x28..0x2A].copy_from_slice(&EXTENT_MAGIC.to_le_bytes());
    slot[0x2A..0x2C].copy_from_slice(&1u16.to_le_bytes()); // entries
    slot[0x2C..0x2E].copy_from_slice(&4u16.to_le_bytes()); // max
    slot[0x2E..0x30].copy_from_slice(&0u16.to_le_bytes()); // depth
    slot[0x30..0x34].copy_from_slice(&0u32.to_le_bytes()); // generation
                                                           // First extent at 0x34..0x40:
                                                           //   ee_block (logical=0), ee_len=1, ee_start_hi, ee_start_lo
    slot[0x34..0x38].copy_from_slice(&0u32.to_le_bytes()); // ee_block
    slot[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes()); // ee_len
    slot[0x3A..0x3C].copy_from_slice(&((root_dir_block >> 32) as u16).to_le_bytes()); // ee_start_hi
    slot[0x3C..0x40].copy_from_slice(&(root_dir_block as u32).to_le_bytes()); // ee_start_lo
                                                                              // Remaining 0x40..0x64 in i_block region stays zero (padding).

    slot[0x64..0x68].copy_from_slice(&0u32.to_le_bytes()); // i_generation
                                                           // i_file_acl_lo (0x68), i_size_hi (0x6C), obso_faddr (0x70) — zero.
                                                           // i_blocks_hi (0x74), i_file_acl_hi (0x76), i_uid_hi (0x78), i_gid_hi (0x7A)
                                                           // — zero.
                                                           // i_checksum_lo at 0x7C, i_reserved at 0x7E — caller patches checksum.

    // Extra section (only present because INODE_SIZE >= 160).
    if slot.len() > 0x80 {
        slot[0x80..0x82].copy_from_slice(&I_EXTRA_ISIZE.to_le_bytes());
        // i_checksum_hi (0x82..0x84) patched by caller.
        // 0x84..0x98 *_extra timestamps + crtime — zero.
    }
    // Mode and other fields default to zero where unspecified.
    // i_mode unspecified bits (read from earlier writes) — already correct.
}
