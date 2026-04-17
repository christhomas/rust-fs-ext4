//! ext4rs — pure-Rust ext4 filesystem driver.
//!
//! Goal: drop-in replacement for `ext4bridge/` (C + lwext4). Exposes the
//! same `ext4rs_*` C functions so the Swift FSKit layer remains
//! unchanged — just relink with `libext4bridge.a` produced from this crate.
//!
//! Architecture (read-only Phase 1):
//! - [`block_io`] — abstract trait for reading device blocks
//! - [`superblock`] — parse + validate the on-disk superblock
//! - [`features`] — feature flag inventory (COMPAT/INCOMPAT/RO_COMPAT)
//! - [`bgd`] — block group descriptor parsing
//! - [`inode`] — inode + extra fields parsing
//! - [`extent`] — extent tree traversal (leaf/internal nodes, uninitialized extents)
//! - [`dir`] — directory entries (linear and HTree)
//! - [`hash`] — htree hash functions (legacy / half_md4 / tea)
//! - [`fs`] — top-level filesystem handle, file/dir lookup, read API
//! - [`capi`] — C ABI exports matching `ext4bridge/ext4_bridge.h`

#![allow(dead_code)] // many spec items not yet wired through

pub mod block_io;
pub mod superblock;
pub mod features;
pub mod bgd;
pub mod inode;
pub mod extent;
pub mod dir;
pub mod hash;
pub mod htree;
pub mod xattr;
pub mod inline_data;
pub mod checksum;
pub mod acl;
pub mod jbd2;
pub mod journal;
pub mod journal_apply;
pub mod alloc;
pub mod extent_mut;
pub mod htree_mut;
pub mod file_mut;
pub mod transaction;
pub mod casefold;
pub mod ea_inode;
pub mod fs;
pub mod file_io;
pub mod path;
pub mod error;

// Always compile the C ABI exports — `libext4bridge.a` must expose the same
// symbols as the C/lwext4 build for drop-in linking.
pub mod capi;

pub use error::{Error, Result};
pub use fs::Filesystem;
pub use superblock::Superblock;
