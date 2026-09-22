#![no_main]
//! One inode, plus the inline xattr area behind it.
//!
//! `i_block` is sixty bytes that mean different things depending on the
//! flags: an extent tree, twelve direct block numbers and three
//! indirect ones, or inline file data.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(inode) = fs_ext4::inode::Inode::parse(data) {
        // The extent header lives in i_block when the extents flag is
        // set, and its own depth and entry count are what the walk
        // trusts.
        // `block` is the sixty-byte area itself.
        let _ = fs_ext4::extent::ExtentHeader::parse(&inode.block);
        let _ = fs_ext4::extent::Extent::parse(&inode.block);
        let _ = fs_ext4::extent::ExtentIdx::parse(&inode.block);
    }
});
