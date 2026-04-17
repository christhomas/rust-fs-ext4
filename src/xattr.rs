//! Extended attribute (xattr) reading.
//!
//! Spec: kernel.org/doc/html/latest/filesystems/ext4/dynamic.html#extended-attributes
//!
//! ext4 stores xattrs in two places:
//!
//! 1. **In-inode** — between the end of the base 128-byte inode + extra_isize
//!    region and the end of the on-disk inode (when inode_size > 128).
//!    Starts with a 4-byte header containing the magic 0xEA020000.
//!
//! 2. **External xattr block** — when more space is needed, `i_file_acl`
//!    (combined hi+lo, 48-bit physical block number) points at a single
//!    block whose layout is: 32-byte header (magic 0xEA020000 + refcount
//!    + ...) followed by `ext4_xattr_entry` records growing forward, with
//!    values stored from the END of the block growing backward.
//!
//! Entry layout (variable size, padded to 4 bytes):
//!   0x00 u8  e_name_len           (length of name in bytes, no NUL)
//!   0x01 u8  e_name_index         (namespace prefix code; see NAME_PREFIX)
//!   0x02 u16 e_value_offs         (offset within the block where value lives)
//!   0x04 u32 e_value_inum         (if EA_INODE feature: inode holding the value)
//!   0x08 u32 e_value_size         (length of value in bytes)
//!   0x0C u32 e_hash               (hash of name + value)
//!   0x10 ..  e_name (e_name_len bytes, no NUL, padded to 4)
//!
//! Phase 1: read-only, in-inode + external block. Hash verification + EA_INODE
//! large-value support deferred.

use crate::block_io::BlockDevice;
use crate::error::{Error, Result};
use crate::inode::Inode;

/// Magic number at the start of an xattr region (in-inode or external block).
pub const EXT4_XATTR_MAGIC: u32 = 0xEA02_0000;

/// Standard namespace prefixes (`e_name_index` value → string).
pub const NAME_PREFIXES: &[(u8, &str)] = &[
    (1, "user."),
    (2, "system.posix_acl_access"),
    (3, "system.posix_acl_default"),
    (4, "trusted."),
    (5, "lustre."),
    (6, "security."),
    (7, "system."),
    (8, "system.richacl"),
];

/// Look up the human-readable prefix for a numeric name_index.
pub fn prefix_for_index(idx: u8) -> Option<&'static str> {
    NAME_PREFIXES.iter().find(|(i, _)| *i == idx).map(|(_, s)| *s)
}

/// One parsed xattr entry: fully-qualified name + raw value bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XattrEntry {
    /// Fully-qualified name, e.g. "user.com.apple.FinderInfo".
    pub name: String,
    /// Raw value bytes (Finder uses binary data, ACLs are binary, etc.).
    pub value: Vec<u8>,
}

/// Read all extended attributes attached to an inode.
///
/// `inode` is the parsed metadata (we need `file_acl` for the external xattr
/// block). `inode_raw` is the on-disk inode bytes (we need bytes past offset
/// 128 + i_extra_isize for the in-inode xattr region).
///
/// Returns entries from in-inode area first, then external xattr block (if any).
/// An inode with no xattrs returns `Ok(vec![])`.
pub fn read_all(
    dev: &dyn BlockDevice,
    inode: &Inode,
    inode_raw: &[u8],
    inode_size: u16,
    block_size: u32,
) -> Result<Vec<XattrEntry>> {
    let mut out = Vec::new();

    // 1. In-inode xattrs: data between end of i_extra_isize area and end of inode.
    if inode_raw.len() >= 128 + 4 {
        let extra_isize = u16::from_le_bytes(inode_raw[128..130].try_into().unwrap()) as usize;
        let xattr_region_start = 128 + extra_isize;
        if xattr_region_start + 4 <= inode_size as usize && xattr_region_start + 4 <= inode_raw.len() {
            let region = &inode_raw[xattr_region_start..(inode_size as usize).min(inode_raw.len())];
            let magic = u32::from_le_bytes(region[..4].try_into().unwrap());
            if magic == EXT4_XATTR_MAGIC {
                // Entries follow the 4-byte magic; values are at e_value_offs from
                // the start of the entry table (== start of region + 4).
                parse_entries(&region[4..], region.len() - 4, &mut out)?;
            }
        }
    }

    // 2. External xattr block: i_file_acl (combined hi+lo) → block number.
    if inode.file_acl != 0 {
        let mut buf = vec![0u8; block_size as usize];
        dev.read_at(inode.file_acl * block_size as u64, &mut buf)?;
        let magic = u32::from_le_bytes(buf[..4].try_into().unwrap());
        if magic != EXT4_XATTR_MAGIC {
            return Err(Error::Corrupt("xattr block magic mismatch"));
        }
        // External block layout: 32-byte header, then entries; values offset
        // is from the start of the block (NOT from end-of-header).
        parse_entries_block(&buf, &mut out)?;
    }

    Ok(out)
}

/// Parse entries from the in-inode xattr area.
///
/// `entries_buf` starts immediately AFTER the 4-byte magic header.
/// In the in-inode format, `e_value_offs` is measured from the start of the
/// entries area (i.e. directly indexes into `entries_buf`). Values are packed
/// backward from the end of the entries area while entries grow forward.
fn parse_entries(entries_buf: &[u8], _region_len: usize, out: &mut Vec<XattrEntry>) -> Result<()> {
    let mut pos = 0;
    while pos + 16 <= entries_buf.len() {
        // Kernel's IS_LAST_ENTRY: the terminator has the full first 4-byte
        // header word all zero. We cannot short-circuit on name_len == 0
        // alone, because ACL xattrs (name_index 2 / 3 for
        // system.posix_acl_{access,default}) legitimately store name_len=0
        // — their full name is implied by the index.
        let header = u32::from_le_bytes(entries_buf[pos..pos + 4].try_into().unwrap());
        if header == 0 {
            break;
        }
        let name_len = entries_buf[pos] as usize;
        let name_index = entries_buf[pos + 1];
        let value_offs =
            u16::from_le_bytes(entries_buf[pos + 2..pos + 4].try_into().unwrap()) as usize;
        let _value_inum = u32::from_le_bytes(entries_buf[pos + 4..pos + 8].try_into().unwrap());
        let value_size =
            u32::from_le_bytes(entries_buf[pos + 8..pos + 12].try_into().unwrap()) as usize;
        // pos+12..pos+16 = e_hash (ignored)

        let entry_size = 16 + name_len;
        let entry_padded = (entry_size + 3) & !3;
        if pos + 16 + name_len > entries_buf.len() {
            return Err(Error::Corrupt("xattr entry name overruns region"));
        }

        let name_bytes = &entries_buf[pos + 16..pos + 16 + name_len];
        let prefix = prefix_for_index(name_index).unwrap_or("");
        let suffix = std::str::from_utf8(name_bytes)
            .map_err(|_| Error::Corrupt("xattr name not utf-8"))?;
        let full_name = format!("{prefix}{suffix}");

        let value = if value_size > 0 {
            if value_offs + value_size > entries_buf.len() {
                return Err(Error::Corrupt("xattr value out of range"));
            }
            entries_buf[value_offs..value_offs + value_size].to_vec()
        } else {
            Vec::new()
        };

        out.push(XattrEntry {
            name: full_name,
            value,
        });

        pos += entry_padded;
    }
    Ok(())
}

/// Parse entries from a full external xattr block.
/// Block layout: 32-byte header at offset 0, then entries starting at 32.
/// `e_value_offs` here is from the START of the block, not the entry table.
fn parse_entries_block(block: &[u8], out: &mut Vec<XattrEntry>) -> Result<()> {
    if block.len() < 32 {
        return Err(Error::Corrupt("xattr block too small"));
    }

    let mut pos = 32; // skip the 32-byte header
    while pos + 16 <= block.len() {
        // See parse_entries: terminator = first 4-byte header word all zero,
        // NOT name_len == 0 (which is legal for ACL entries).
        let header = u32::from_le_bytes(block[pos..pos + 4].try_into().unwrap());
        if header == 0 {
            break;
        }
        let name_len = block[pos] as usize;
        let name_index = block[pos + 1];
        let value_offs = u16::from_le_bytes(block[pos + 2..pos + 4].try_into().unwrap()) as usize;
        let _value_inum = u32::from_le_bytes(block[pos + 4..pos + 8].try_into().unwrap());
        let value_size = u32::from_le_bytes(block[pos + 8..pos + 12].try_into().unwrap()) as usize;

        let entry_size = 16 + name_len;
        let entry_padded = (entry_size + 3) & !3;
        if pos + 16 + name_len > block.len() {
            return Err(Error::Corrupt("xattr entry name overruns block"));
        }

        let name_bytes = &block[pos + 16..pos + 16 + name_len];
        let prefix = prefix_for_index(name_index).unwrap_or("");
        let suffix = std::str::from_utf8(name_bytes)
            .map_err(|_| Error::Corrupt("xattr name not utf-8"))?;
        let full_name = format!("{prefix}{suffix}");

        let value = if value_size > 0 {
            if value_offs + value_size > block.len() {
                return Err(Error::Corrupt("xattr block value out of range"));
            }
            block[value_offs..value_offs + value_size].to_vec()
        } else {
            Vec::new()
        };

        out.push(XattrEntry {
            name: full_name,
            value,
        });

        pos += entry_padded;
    }
    Ok(())
}

/// Convenience: get a single xattr value by name. Returns `None` if not present.
pub fn get(
    dev: &dyn BlockDevice,
    inode: &Inode,
    inode_raw: &[u8],
    inode_size: u16,
    block_size: u32,
    name: &str,
) -> Result<Option<Vec<u8>>> {
    let all = read_all(dev, inode, inode_raw, inode_size, block_size)?;
    Ok(all.into_iter().find(|e| e.name == name).map(|e| e.value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_known() {
        assert_eq!(prefix_for_index(1), Some("user."));
        assert_eq!(prefix_for_index(7), Some("system."));
        assert_eq!(prefix_for_index(99), None);
    }
}
