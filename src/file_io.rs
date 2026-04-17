//! Read file (or directory) contents using extent traversal.
//!
//! Composes `inode::Inode` + `extent::lookup` + `block_io::BlockDevice` into
//! a Read/Seek-style API. Phase 1 read-only.

use crate::error::{Error, Result};
use crate::extent;
use crate::fs::Filesystem;
use crate::inline_data;
use crate::inode::{Inode, InodeFlags};

/// Read up to `length` bytes from `inode` starting at byte `offset`.
/// Returns the actual number of bytes read (may be less than requested
/// if EOF is reached).
///
/// Sparse holes and uninitialised extents are returned as zero bytes.
///
/// Files with `EXT4_INLINE_DATA_FL` cannot go through this function — their
/// content lives in the inode's xattr region which the parsed `Inode` doesn't
/// carry. Callers must use [`read_with_raw`] or [`read_inline`] instead;
/// this function returns `Err(Corrupt)` on inline inodes.
pub fn read(
    fs: &Filesystem,
    inode: &Inode,
    offset: u64,
    length: u64,
    out: &mut [u8],
) -> Result<u64> {
    if length == 0 || out.is_empty() {
        return Ok(0);
    }

    // Inline data: file lives in i_block (60 bytes) + system.data xattr overflow.
    // Caller must use [`read_inline`] for inline files since we need the raw
    // inode bytes (xattr region) which the parsed Inode no longer carries.
    if (inode.flags & InodeFlags::INLINE_DATA.bits()) != 0 {
        return Err(Error::Corrupt(
            "inline-data file: caller must use file_io::read_inline with raw inode bytes",
        ));
    }

    // Without EXTENTS, fall back would be the legacy indirect block scheme.
    // We don't support that in Phase 1 — modern mkfs always uses extents.
    if (inode.flags & InodeFlags::EXTENTS.bits()) == 0 {
        return Err(Error::Corrupt(
            "legacy indirect blocks not supported (use extents)",
        ));
    }

    // Clamp length to file size.
    if offset >= inode.size {
        return Ok(0);
    }
    let max_read = (inode.size - offset).min(length).min(out.len() as u64);
    if max_read == 0 {
        return Ok(0);
    }

    let block_size = fs.sb.block_size() as u64;
    let mut written: u64 = 0;
    let mut cur_offset = offset;
    let end_offset = offset + max_read;

    // Walk byte-by-block until we've satisfied the request.
    while cur_offset < end_offset {
        let logical_block = cur_offset / block_size;
        let off_in_block = (cur_offset % block_size) as usize;
        let bytes_available_in_block = block_size as usize - off_in_block;
        let bytes_remaining = (end_offset - cur_offset) as usize;
        let copy_len = bytes_available_in_block.min(bytes_remaining);

        let dst = &mut out[written as usize..written as usize + copy_len];

        match extent::lookup(
            &inode.block,
            fs.dev.as_ref(),
            fs.sb.block_size(),
            logical_block,
        )? {
            None => {
                // Sparse hole — fill with zeros.
                dst.fill(0);
            }
            Some(ext) if ext.uninitialized => {
                // Pre-allocated but unwritten — also reads as zeros.
                dst.fill(0);
            }
            Some(ext) => {
                // Map logical to physical and read.
                let physical_block = ext.map(logical_block);
                let phys_byte_offset = physical_block * block_size + off_in_block as u64;
                fs.dev.read_at(phys_byte_offset, dst)?;
            }
        }

        written += copy_len as u64;
        cur_offset += copy_len as u64;
    }

    Ok(written)
}

/// Convenience: read an entire file into a freshly-allocated Vec.
/// Caps at `inode.size` — files larger than `usize::MAX` will panic.
///
/// For inline-data files, callers should use [`read_all_with_raw`] to provide
/// the raw inode bytes (needed to read the in-inode xattr region holding
/// data overflow).
pub fn read_all(fs: &Filesystem, inode: &Inode) -> Result<Vec<u8>> {
    let size = inode.size as usize;
    let mut buf = vec![0u8; size];
    let n = read(fs, inode, 0, size as u64, &mut buf)?;
    buf.truncate(n as usize);
    Ok(buf)
}

/// Read an inline-data file. Equivalent to `read_all` but for files with
/// `EXT4_INLINE_DATA_FL` set — needs the raw on-disk inode bytes since the
/// data overflow lives in the in-inode xattr region.
pub fn read_inline(fs: &Filesystem, inode: &Inode, inode_raw: &[u8]) -> Result<Vec<u8>> {
    inline_data::read_all(
        fs.dev.as_ref(),
        inode,
        inode_raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
    )
}

/// Read a range from any file (inline or extent-backed). Use this when you
/// already have the raw inode bytes (and don't want to special-case inline
/// at the call site).
pub fn read_with_raw(
    fs: &Filesystem,
    inode: &Inode,
    inode_raw: &[u8],
    offset: u64,
    length: u64,
    out: &mut [u8],
) -> Result<u64> {
    if (inode.flags & InodeFlags::INLINE_DATA.bits()) != 0 {
        let n = inline_data::read_range(
            fs.dev.as_ref(),
            inode,
            inode_raw,
            fs.sb.inode_size,
            fs.sb.block_size(),
            offset,
            out,
        )?;
        return Ok(n as u64);
    }
    read(fs, inode, offset, length, out)
}

/// Read a range from an extent-backed file with extent-tail CRC verification.
///
/// Identical to [`read`] but each off-inode extent index/leaf block traversed
/// during the lookup is CRC-verified using `(ino, inode.generation, fs.csum)`.
/// A mismatch aborts the read with `Error::BadChecksum`.
///
/// Inline-data files must use [`read_with_raw_verified`] (or the unverified
/// `read_inline`) — extent verification is meaningless without an extent tree.
pub fn read_verified(
    fs: &Filesystem,
    inode: &Inode,
    ino: u32,
    offset: u64,
    length: u64,
    out: &mut [u8],
) -> Result<u64> {
    if length == 0 || out.is_empty() {
        return Ok(0);
    }
    if (inode.flags & InodeFlags::INLINE_DATA.bits()) != 0 {
        return Err(Error::Corrupt(
            "inline-data file: caller must use file_io::read_inline with raw inode bytes",
        ));
    }
    if (inode.flags & InodeFlags::EXTENTS.bits()) == 0 {
        return Err(Error::Corrupt(
            "legacy indirect blocks not supported (use extents)",
        ));
    }
    if offset >= inode.size {
        return Ok(0);
    }
    let max_read = (inode.size - offset).min(length).min(out.len() as u64);
    if max_read == 0 {
        return Ok(0);
    }

    let block_size = fs.sb.block_size() as u64;
    let bs32 = fs.sb.block_size();
    let ctx = extent::ExtentVerifyCtx {
        ino,
        generation: inode.generation,
        csum: &fs.csum,
    };
    let mut written: u64 = 0;
    let mut cur_offset = offset;
    let end_offset = offset + max_read;

    while cur_offset < end_offset {
        let logical_block = cur_offset / block_size;
        let off_in_block = (cur_offset % block_size) as usize;
        let bytes_available_in_block = block_size as usize - off_in_block;
        let bytes_remaining = (end_offset - cur_offset) as usize;
        let copy_len = bytes_available_in_block.min(bytes_remaining);
        let dst = &mut out[written as usize..written as usize + copy_len];

        match extent::lookup_verified(
            &inode.block,
            fs.dev.as_ref(),
            bs32,
            logical_block,
            Some(&ctx),
        )? {
            None => dst.fill(0),
            Some(ext) if ext.uninitialized => dst.fill(0),
            Some(ext) => {
                let physical_block = ext.map(logical_block);
                let phys_byte_offset = physical_block * block_size + off_in_block as u64;
                fs.dev.read_at(phys_byte_offset, dst)?;
            }
        }
        written += copy_len as u64;
        cur_offset += copy_len as u64;
    }
    Ok(written)
}

/// Read range from any file with verification of extent blocks (when the
/// file is extent-backed and `fs.csum.enabled`). Inline-data files dispatch
/// straight to `inline_data::read_range` — there's nothing to verify in that
/// path beyond the inode itself, which the caller should already have read
/// via `Filesystem::read_inode_verified`.
pub fn read_with_raw_verified(
    fs: &Filesystem,
    inode: &Inode,
    inode_raw: &[u8],
    ino: u32,
    offset: u64,
    length: u64,
    out: &mut [u8],
) -> Result<u64> {
    if (inode.flags & InodeFlags::INLINE_DATA.bits()) != 0 {
        let n = inline_data::read_range(
            fs.dev.as_ref(),
            inode,
            inode_raw,
            fs.sb.inode_size,
            fs.sb.block_size(),
            offset,
            out,
        )?;
        return Ok(n as u64);
    }
    read_verified(fs, inode, ino, offset, length, out)
}
