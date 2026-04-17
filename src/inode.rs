//! ext4 inode parsing.
//!
//! Spec: docs/ext4-spec/inodes-extents.md
//!
//! Base inode is 128 bytes; modern ext4 with EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE
//! adds another 32 bytes (i_extra_isize) for a total of 160 bytes. All fields
//! little-endian. The high halves of uid/gid/size/file_acl/blocks/checksum live
//! at the end of the base 128 bytes; nanosecond timestamps + crtime live in the
//! extra section.

use crate::error::{Error, Result};

/// Minimum on-disk inode size (rev 0).
pub const INODE_BASE_SIZE: usize = 128;
/// Offset where the i_extra_isize field begins (start of extra section).
pub const INODE_EXTRA_OFFSET: usize = 128;

// POSIX file-type bits (high nibble of i_mode).
pub const S_IFMT: u16 = 0xF000;
pub const S_IFREG: u16 = 0x8000;
pub const S_IFDIR: u16 = 0x4000;
pub const S_IFLNK: u16 = 0xA000;
pub const S_IFBLK: u16 = 0x6000;
pub const S_IFCHR: u16 = 0x2000;
pub const S_IFIFO: u16 = 0x1000;
pub const S_IFSOCK: u16 = 0xC000;

bitflags::bitflags! {
    /// `i_flags` — per-inode behaviour flags.
    /// Spec: kernel.org/doc/html/latest/filesystems/ext4/inodes.html
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InodeFlags: u32 {
        /// Secure deletion (unused).
        const SECRM        = 0x0000_0001;
        /// Undelete (unused).
        const UNRM         = 0x0000_0002;
        /// Compressed file.
        const COMPR        = 0x0000_0004;
        /// Synchronous writes.
        const SYNC         = 0x0000_0008;
        /// Immutable.
        const IMMUTABLE    = 0x0000_0010;
        /// Append-only.
        const APPEND       = 0x0000_0020;
        /// Do not dump.
        const NODUMP       = 0x0000_0040;
        /// Do not update access time.
        const NOATIME      = 0x0000_0080;
        /// Hash-tree-indexed directory.
        const INDEX        = 0x0000_1000;
        /// File data stored in extended attributes.
        const EA_INODE     = 0x0020_0000;
        /// Inode uses extents (EXT4_EXTENTS_FL).
        const EXTENTS      = 0x0008_0000;
        /// Inode stores a huge file (i_blocks counted in fs blocks not 512B sectors).
        const HUGE_FILE    = 0x0004_0000;
        /// Inline data — file contents live inside i_block + xattrs.
        const INLINE_DATA  = 0x1000_0000;
        /// Alias for EXTENTS (matches kernel naming `EXT4_EXTENTS_FL`).
        const EXTENT       = 0x0008_0000;
        /// Inode has extra (nanosecond) timestamp fields.
        const EXTRA_ATIME  = 0x0000_0100;
    }
}

/// Parsed ext4 inode.
///
/// Combines hi+lo halves for uid, gid, size, file_acl, blocks, and checksum so
/// callers don't have to reassemble them. Nanosecond timestamps come from the
/// `*_extra` fields when present (top 30 bits = nsec, low 2 bits = epoch).
#[derive(Debug, Clone)]
pub struct Inode {
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: u32,
    pub mtime: u32,
    pub ctime: u32,
    pub dtime: u32,
    pub crtime: u32,
    pub atime_nsec: u32,
    pub mtime_nsec: u32,
    pub ctime_nsec: u32,
    pub crtime_nsec: u32,
    pub links_count: u16,
    pub blocks: u64,         // 512-byte sectors (per spec; HUGE_FILE flag changes meaning)
    pub flags: u32,
    /// Raw 60-byte i_block area — extent header / direct pointers / inline data.
    /// Parsed by the extent module.
    pub block: [u8; 60],
    pub generation: u32,
    pub file_acl: u64,
    pub checksum: u32,
}

impl Inode {
    /// Parse an inode from its on-disk bytes.
    /// Accepts any length >= 128; if >= 160 and i_extra_isize >= 28, parses the
    /// extra (nsec + crtime + checksum_hi) section as well.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        if raw.len() < INODE_BASE_SIZE {
            return Err(Error::Corrupt("inode buffer too small"));
        }

        let mode          = u16::from_le_bytes(raw[0x00..0x02].try_into().unwrap());
        let uid_lo        = u16::from_le_bytes(raw[0x02..0x04].try_into().unwrap());
        let size_lo       = u32::from_le_bytes(raw[0x04..0x08].try_into().unwrap());
        let atime         = u32::from_le_bytes(raw[0x08..0x0C].try_into().unwrap());
        let ctime         = u32::from_le_bytes(raw[0x0C..0x10].try_into().unwrap());
        let mtime         = u32::from_le_bytes(raw[0x10..0x14].try_into().unwrap());
        let dtime         = u32::from_le_bytes(raw[0x14..0x18].try_into().unwrap());
        let gid_lo        = u16::from_le_bytes(raw[0x18..0x1A].try_into().unwrap());
        let links_count   = u16::from_le_bytes(raw[0x1A..0x1C].try_into().unwrap());
        let blocks_lo     = u32::from_le_bytes(raw[0x1C..0x20].try_into().unwrap());
        let flags         = u32::from_le_bytes(raw[0x20..0x24].try_into().unwrap());
        // 0x24..0x28 is i_osd1 (Linux: i_version_lo) — ignored here.

        let mut block = [0u8; 60];
        block.copy_from_slice(&raw[0x28..0x64]);

        let generation    = u32::from_le_bytes(raw[0x64..0x68].try_into().unwrap());
        let file_acl_lo   = u32::from_le_bytes(raw[0x68..0x6C].try_into().unwrap());
        let size_hi       = u32::from_le_bytes(raw[0x6C..0x70].try_into().unwrap());
        // 0x70..0x74 obso_faddr ignored.
        let blocks_hi     = u16::from_le_bytes(raw[0x74..0x76].try_into().unwrap());
        let file_acl_hi   = u16::from_le_bytes(raw[0x76..0x78].try_into().unwrap());
        let uid_hi        = u16::from_le_bytes(raw[0x78..0x7A].try_into().unwrap());
        let gid_hi        = u16::from_le_bytes(raw[0x7A..0x7C].try_into().unwrap());
        let checksum_lo   = u16::from_le_bytes(raw[0x7C..0x7E].try_into().unwrap());
        // 0x7E..0x80 i_reserved2.

        // Defaults (when no extra section present).
        let mut atime_nsec  = 0u32;
        let mut mtime_nsec  = 0u32;
        let mut ctime_nsec  = 0u32;
        let mut crtime_nsec = 0u32;
        let mut crtime      = 0u32;
        let mut checksum_hi = 0u16;

        // Extra fields — only present when on-disk inode size is >= 160 AND
        // i_extra_isize covers them (>= 28 includes through i_projid; we read
        // what we need at >= 24 to cover up to crtime_extra).
        if raw.len() >= INODE_EXTRA_OFFSET + 4 {
            let i_extra_isize = u16::from_le_bytes(raw[0x80..0x82].try_into().unwrap());
            // Sanity: i_extra_isize is the number of bytes beyond the 128-byte
            // base that are valid. Must fit inside the on-disk inode.
            let extra_end = INODE_EXTRA_OFFSET + i_extra_isize as usize;
            if extra_end > raw.len() {
                return Err(Error::Corrupt("i_extra_isize exceeds inode size"));
            }

            // Read each extra field only if i_extra_isize covers it.
            // Layout (offset from inode start):
            //   0x80 u16 i_extra_isize
            //   0x82 u16 i_checksum_hi          (needs >= 4)
            //   0x84 u32 i_ctime_extra          (needs >= 8)
            //   0x88 u32 i_mtime_extra          (needs >= 12)
            //   0x8C u32 i_atime_extra          (needs >= 16)
            //   0x90 u32 i_crtime               (needs >= 20)
            //   0x94 u32 i_crtime_extra         (needs >= 24)
            if i_extra_isize >= 4 {
                checksum_hi = u16::from_le_bytes(raw[0x82..0x84].try_into().unwrap());
            }
            if i_extra_isize >= 8 {
                ctime_nsec = u32::from_le_bytes(raw[0x84..0x88].try_into().unwrap()) >> 2;
            }
            if i_extra_isize >= 12 {
                mtime_nsec = u32::from_le_bytes(raw[0x88..0x8C].try_into().unwrap()) >> 2;
            }
            if i_extra_isize >= 16 {
                atime_nsec = u32::from_le_bytes(raw[0x8C..0x90].try_into().unwrap()) >> 2;
            }
            if i_extra_isize >= 20 {
                crtime = u32::from_le_bytes(raw[0x90..0x94].try_into().unwrap());
            }
            if i_extra_isize >= 24 {
                crtime_nsec = u32::from_le_bytes(raw[0x94..0x98].try_into().unwrap()) >> 2;
            }
        }

        Ok(Self {
            mode,
            uid: ((uid_hi as u32) << 16) | uid_lo as u32,
            gid: ((gid_hi as u32) << 16) | gid_lo as u32,
            size: ((size_hi as u64) << 32) | size_lo as u64,
            atime,
            mtime,
            ctime,
            dtime,
            crtime,
            atime_nsec,
            mtime_nsec,
            ctime_nsec,
            crtime_nsec,
            links_count,
            blocks: ((blocks_hi as u64) << 32) | blocks_lo as u64,
            flags,
            block,
            generation,
            file_acl: ((file_acl_hi as u64) << 32) | file_acl_lo as u64,
            checksum: ((checksum_hi as u32) << 16) | checksum_lo as u32,
        })
    }

    /// File type from i_mode.
    pub fn file_type(&self) -> u16 {
        self.mode & S_IFMT
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == S_IFDIR
    }

    pub fn is_file(&self) -> bool {
        self.file_type() == S_IFREG
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == S_IFLNK
    }

    /// True when EXT4_EXTENTS_FL is set in i_flags — i_block holds an extent
    /// tree rather than legacy direct/indirect block pointers.
    pub fn has_extents(&self) -> bool {
        self.flags & InodeFlags::EXTENTS.bits() != 0
    }

    /// True when INLINE_DATA flag is set — file contents live inside i_block.
    pub fn has_inline_data(&self) -> bool {
        self.flags & InodeFlags::INLINE_DATA.bits() != 0
    }

    /// Decode i_flags into a typed bitflags value (silently drops unknown bits).
    pub fn flag_set(&self) -> InodeFlags {
        InodeFlags::from_bits_truncate(self.flags)
    }
}
