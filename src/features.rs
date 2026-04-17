//! ext4 feature flags — COMPAT, INCOMPAT, RO_COMPAT.
//!
//! Spec source: kernel.org/doc/html/latest/filesystems/ext4/super.html

use bitflags::bitflags;

bitflags! {
    /// COMPAT features — safe to ignore if unknown.
    #[derive(Debug, Clone, Copy)]
    pub struct Compat: u32 {
        const DIR_PREALLOC      = 0x0001;
        const IMAGIC_INODES     = 0x0002;
        const HAS_JOURNAL       = 0x0004;
        const EXT_ATTR          = 0x0008;
        const RESIZE_INODE      = 0x0010;
        const DIR_INDEX         = 0x0020;  // HTree
        const LAZY_BG           = 0x0040;
        const SPARSE_SUPER2     = 0x0200;
        const FAST_COMMIT       = 0x0400;
        const ORPHAN_FILE       = 0x1000;
    }

    /// INCOMPAT features — kernel MUST understand or refuse to mount.
    #[derive(Debug, Clone, Copy)]
    pub struct Incompat: u32 {
        const COMPRESSION       = 0x00001;
        const FILETYPE          = 0x00002;
        const RECOVER           = 0x00004;
        const JOURNAL_DEV       = 0x00008;
        const META_BG           = 0x00010;
        const EXTENTS           = 0x00040;
        const BIT64             = 0x00080;
        const MMP               = 0x00100;
        const FLEX_BG           = 0x00200;
        const EA_INODE          = 0x00400;
        const DIRDATA           = 0x01000;
        const CSUM_SEED         = 0x02000;
        const LARGEDIR          = 0x04000;
        const INLINE_DATA       = 0x08000;
        const ENCRYPT           = 0x10000;
        const CASEFOLD          = 0x20000;
    }

    /// RO_COMPAT features — must mount read-only if unknown.
    #[derive(Debug, Clone, Copy)]
    pub struct RoCompat: u32 {
        const SPARSE_SUPER      = 0x0001;
        const LARGE_FILE        = 0x0002;
        const BTREE_DIR         = 0x0004;
        const HUGE_FILE         = 0x0008;
        const GDT_CSUM          = 0x0010;
        const DIR_NLINK         = 0x0020;
        const EXTRA_ISIZE       = 0x0040;
        const HAS_SNAPSHOT      = 0x0080;
        const QUOTA             = 0x0100;
        const BIGALLOC          = 0x0200;
        const METADATA_CSUM     = 0x0400;
        const REPLICA           = 0x0800;
        const READONLY          = 0x1000;
        const PROJECT           = 0x2000;
        const VERITY            = 0x8000;
        const ORPHAN_PRESENT    = 0x10000;
    }
}

/// INCOMPAT bits we know how to handle (Phase 1 read-only goal).
/// Anything else in feature_incompat means refuse-to-mount.
pub const SUPPORTED_INCOMPAT: u32 = 0
    | Incompat::FILETYPE.bits()
    | Incompat::EXTENTS.bits()
    | Incompat::BIT64.bits()
    | Incompat::FLEX_BG.bits()
    | Incompat::CSUM_SEED.bits()
    // The features below are tolerated for read-only mount even if not fully implemented:
    | Incompat::RECOVER.bits()      // we'll skip journal replay for now (warn)
    | Incompat::MMP.bits()          // ignore for read-only
    | Incompat::INLINE_DATA.bits()  // we'll handle the flag, even if data overflow uses xattr later
    | Incompat::LARGEDIR.bits()
    | Incompat::EA_INODE.bits()
    | Incompat::CASEFOLD.bits()
    ;

/// RO_COMPAT bits we tolerate (since we mount read-only anyway).
pub const SUPPORTED_RO_COMPAT: u32 = 0
    | RoCompat::SPARSE_SUPER.bits()
    | RoCompat::LARGE_FILE.bits()
    | RoCompat::HUGE_FILE.bits()
    | RoCompat::GDT_CSUM.bits()
    | RoCompat::DIR_NLINK.bits()
    | RoCompat::EXTRA_ISIZE.bits()
    | RoCompat::QUOTA.bits()
    | RoCompat::METADATA_CSUM.bits()
    | RoCompat::PROJECT.bits()
    | RoCompat::ORPHAN_PRESENT.bits()
    | RoCompat::BIGALLOC.bits()  // tolerated; cluster math may need updates
    ;

/// Check whether the filesystem can be mounted read-only.
/// Returns Err with the unsupported bits if not.
pub fn check_mountable(feature_incompat: u32, _feature_ro_compat: u32) -> crate::error::Result<()> {
    let unsupported_incompat = feature_incompat & !SUPPORTED_INCOMPAT;
    if unsupported_incompat != 0 {
        return Err(crate::error::Error::UnsupportedIncompat(unsupported_incompat));
    }
    // RO_COMPAT bits are all OK for read-only mount even if unknown,
    // per the spec's compatibility model. We log them but don't fail.
    Ok(())
}
