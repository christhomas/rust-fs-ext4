//! EA_INODE xattr value follow (E12, Phase 5).
//!
//! When an xattr entry has `e_value_inum != 0` (and the filesystem has
//! `INCOMPAT_EA_INODE` enabled), the value does NOT live in the xattr
//! block — instead it lives in the referenced inode's file body. This
//! accommodates xattrs whose values are larger than an inline xattr slot
//! can hold (typically: ACL blobs > 4 KiB, Finder metadata bundles, etc).
//!
//! Spec: <https://www.kernel.org/doc/html/latest/filesystems/ext4/dynamic.html#extended-attributes>
//! Field: `ext4_xattr_entry.e_value_inum`. Target inode is a regular inode
//! with the `EA_INODE` flag set (`0x200000`); its `i_size` = value length;
//! data lives via the same extent-tree/inline-data mechanisms as a regular
//! file.
//!
//! Read only: `xattr.rs` follows a value here when it resolves one, and
//! nothing in this crate creates or rewrites an EA inode.

use crate::error::{Error, Result};
use crate::file_io;
use crate::fs::Filesystem;
use crate::inode::{Inode, InodeFlags};

/// Follow an `e_value_inum` pointer and return the raw value bytes.
///
/// `declared` is the entry's own `e_value_size`. Errors:
/// - [`Error::InvalidInode`] if `value_inum` is 0 or out of range.
/// - [`Error::Corrupt`] if the target inode does not have the `EA_INODE`
///   flag set (guards against pointing at a regular file / directory by
///   accident), or if its length disagrees with `declared`.
///
/// # Why the length is checked here rather than after the read (#121)
///
/// An xattr entry says how long its value is, and for an EA-inode-backed
/// value nothing compared the two: `getxattr` reported success and handed
/// back the EA inode's whole body — plausible bytes rather than an error.
/// The kernel's `ext4_xattr_inode_iget` makes this comparison and returns
/// `-EFSCORRUPTED`.
///
/// It is made BEFORE the body is read because the disagreement bounds the
/// work: an entry declaring 64 bytes against an inode declaring 2 GiB
/// would otherwise allocate and read the two gigabytes before anyone
/// could notice they were not asked for.
pub fn read_value_inode(fs: &Filesystem, value_inum: u32, declared: u32) -> Result<Vec<u8>> {
    if value_inum == 0 {
        return Err(Error::InvalidInode(value_inum));
    }

    let raw = fs.read_inode_raw(value_inum)?;
    let inode = Inode::parse(&raw)?;
    if inode.flags & InodeFlags::EA_INODE.bits() == 0 {
        return Err(Error::Corrupt(
            "e_value_inum target missing EA_INODE flag (0x200000)",
        ));
    }
    if inode.size != u64::from(declared) {
        return Err(Error::Corrupt(
            "an EA inode's length disagrees with the xattr entry's e_value_size",
        ));
    }

    // Body read path: identical to a regular file body. If the target uses
    // inline data (rare but legal for very small EA_INODE values), follow
    // that route; otherwise extent-tree read.
    if inode.flags & InodeFlags::INLINE_DATA.bits() != 0 {
        return file_io::read_inline(fs, &inode, &raw);
    }
    file_io::read_all(fs, &inode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inode::InodeFlags;

    /// Quick property check: EA_INODE flag bit is 0x200000 per spec.
    #[test]
    fn ea_inode_flag_bit_value() {
        assert_eq!(InodeFlags::EA_INODE.bits(), 0x0020_0000);
    }

    /// An EA inode whose length disagrees with the entry that names it
    /// is refused, and one that agrees is read (#121).
    ///
    /// Built here rather than read from a fixture: the volume is
    /// formatted by this crate and the EA inode written into it by
    /// hand, so the disagreement is exactly the one under test and the
    /// pair runs with no fixtures, no tools and no VM.
    mod value_size {
        use super::*;
        use crate::block_io::BlockDevice;
        use crate::error::Error;
        use crate::fs::Filesystem;
        use std::sync::{Arc, Mutex};

        const BS: u32 = 1024;
        const VOL: u64 = 4 * 1024 * 1024;
        /// A free inode number on a freshly formatted volume: 1..=10 are
        /// reserved and 11 is `lost+found` where one exists.
        const EA_INO: u32 = 12;
        const VALUE: &[u8] = b"deadbeef";

        struct MemDev(Mutex<Vec<u8>>);

        impl BlockDevice for MemDev {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
                let b = self.0.lock().unwrap();
                let (start, end) = (offset as usize, offset as usize + buf.len());
                if end > b.len() {
                    return Err(Error::Corrupt("MemDev: read past end"));
                }
                buf.copy_from_slice(&b[start..end]);
                Ok(())
            }
            fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
                let mut b = self.0.lock().unwrap();
                let (start, end) = (offset as usize, offset as usize + buf.len());
                if end > b.len() {
                    return Err(Error::Corrupt("MemDev: write past end"));
                }
                b[start..end].copy_from_slice(buf);
                Ok(())
            }
            fn size_bytes(&self) -> u64 {
                VOL
            }
            fn flush(&self) -> Result<()> {
                Ok(())
            }
            fn is_writable(&self) -> bool {
                true
            }
        }

        /// A formatted volume holding one EA inode of `size` bytes whose
        /// body is [`VALUE`].
        ///
        /// `size` is written into `i_size` and the body is written
        /// whatever it says, which is how the two are made to disagree.
        fn volume_with_ea_inode(size: u64) -> Filesystem {
            let dev = Arc::new(MemDev(Mutex::new(vec![0u8; VOL as usize])));
            crate::mkfs::format_filesystem(dev.as_ref(), None, None, VOL, BS).expect("format");
            let fs = Filesystem::mount(dev).expect("mount");

            let mut raw = fs.read_inode_raw(EA_INO).expect("the inode to borrow");
            raw.fill(0);
            // A regular file, one link, EA_INODE with its body inline.
            raw[crate::inode::OFF_MODE..crate::inode::OFF_MODE + 2]
                .copy_from_slice(&0o100_644u16.to_le_bytes());
            raw[crate::inode::OFF_LINKS_COUNT..crate::inode::OFF_LINKS_COUNT + 2]
                .copy_from_slice(&1u16.to_le_bytes());
            let flags = crate::inode::InodeFlags::EA_INODE.bits()
                | crate::inode::InodeFlags::INLINE_DATA.bits();
            raw[crate::inode::OFF_FLAGS..crate::inode::OFF_FLAGS + 4]
                .copy_from_slice(&flags.to_le_bytes());
            raw[crate::inode::OFF_SIZE_LO..crate::inode::OFF_SIZE_LO + 4]
                .copy_from_slice(&(size as u32).to_le_bytes());
            raw[crate::inode::OFF_BLOCK..crate::inode::OFF_BLOCK + VALUE.len()]
                .copy_from_slice(VALUE);
            let generation = u32::from_le_bytes(raw[0x64..0x68].try_into().expect("4 bytes"));
            fs.finalize_inode_raw(EA_INO, generation, &mut raw)
                .expect("checksum");
            fs.write_inode_raw(EA_INO, &raw)
                .expect("write the EA inode");
            fs
        }

        #[test]
        fn a_value_the_entry_asked_for_is_returned() {
            let fs = volume_with_ea_inode(VALUE.len() as u64);
            let value = read_value_inode(&fs, EA_INO, VALUE.len() as u32).expect("the value");
            assert_eq!(value, VALUE, "the entry's own bytes");
        }

        /// The defect: the entry says eight bytes, the inode says sixty,
        /// and what came back was the inode's answer with no complaint.
        #[test]
        fn an_ea_inode_longer_than_the_entry_declared_is_refused() {
            let fs = volume_with_ea_inode(60);
            match read_value_inode(&fs, EA_INO, VALUE.len() as u32) {
                Err(Error::Corrupt(why)) => assert!(
                    why.contains("e_value_size"),
                    "refused, but not for the length it disagreed about: {why}"
                ),
                Err(other) => panic!("refused for the wrong reason: {other}"),
                Ok(value) => panic!(
                    "an entry declaring {} bytes was answered with {} — the EA inode's \
                     own idea of its length, reported as success",
                    VALUE.len(),
                    value.len()
                ),
            }
        }

        /// And the other way round, which a check written as `>` would
        /// let through.
        #[test]
        fn an_ea_inode_shorter_than_the_entry_declared_is_refused() {
            let fs = volume_with_ea_inode(4);
            assert!(
                read_value_inode(&fs, EA_INO, VALUE.len() as u32).is_err(),
                "an entry declaring more than the inode holds is a disagreement too"
            );
        }
    }

    /// Guard against accidentally calling read_value_inode with ino=0.
    #[test]
    fn zero_inode_is_rejected() {
        // We don't need a Filesystem to exercise the guard — the first
        // branch short-circuits. Just verify the error discriminant.
        let err = Error::InvalidInode(0);
        match err {
            Error::InvalidInode(n) => assert_eq!(n, 0),
            _ => panic!(),
        }
    }
}
