#![no_main]
//! The superblock, read before anything is known. The block size is a
//! shift, the inode size and inodes-per-group divide every inode
//! number, and the feature masks decide which later decoder runs.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = fs_ext4::Superblock::parse(data.to_vec());
});
