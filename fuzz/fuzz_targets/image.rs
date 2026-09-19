#![no_main]
//! A whole filesystem, mounted and read.
//!
//! This is the target with the most reach: the superblock, the group
//! descriptors, the inode table, extent trees, htree indexes and the
//! journal are each read from an offset the one before it supplied, and
//! only mounting reaches all of them.
//!
//! `replay_journal_if_dirty` runs inside the walk, deliberately. A
//! journal is a structure the format expects to be partially written --
//! that is its purpose -- so it is parsed with a corruption tolerance
//! the other structures do not have, and it runs at mount, before
//! anything has been established.
use fs_ext4_fuzz::walk;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    walk(data);
});
