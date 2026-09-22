#![no_main]
//! A directory block: a chain of variable-length records, each
//! declaring where the next one begins. A record length of zero is the
//! classic way to make that chain never end.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for has_file_type in [true, false] {
        let _ = fs_ext4::dir::parse_block(data, has_file_type);
    }
    let _ = fs_ext4::dir::has_csum_tail(data);
    let _ = fs_ext4::htree::parse_root_info(data);
    let _ = fs_ext4::htree::parse_root_entries(data);
    let _ = fs_ext4::htree::parse_node_entries(data);
});
