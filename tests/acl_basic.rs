//! POSIX ACL decoding tests against test-disks/ext4-acl.img.
//!
//! Image layout (see build-ext4-feature-images.sh build_acl):
//!   /mode_only.txt  — u::rwx g::r-x o::r--  (3 short entries, no MASK)
//!   /named.txt      — u::rwx u:1000:rw- g::r-x g:2000:r-- m::rwx o::r--
//!   /acl_dir/       — access + default ACL
//!   /plain.txt      — no ACL
//!
//! These tests are skipped when the image is not present (the Docker builder
//! is macOS-unavailable without docker), matching @5's pattern.

use ext4rs::acl::{self, AclKind, AclTag, ACL_EXECUTE, ACL_READ, ACL_WRITE};
use ext4rs::bgd;
use ext4rs::block_io::{BlockDevice, FileDevice};
use ext4rs::error::Result;
use ext4rs::fs::Filesystem;
use ext4rs::inode::Inode;
use ext4rs::path;
use ext4rs::xattr;
use std::path::Path;
use std::sync::Arc;

const TEST_IMAGE: &str = "test-disks/ext4-acl.img";

fn open_or_skip() -> Option<(Arc<dyn BlockDevice>, Filesystem)> {
    if !Path::new(TEST_IMAGE).exists() {
        eprintln!("skip: {TEST_IMAGE} not built; run test-disks/build-ext4-feature-images.sh acl");
        return None;
    }
    let dev = Arc::new(FileDevice::open(TEST_IMAGE).expect("open acl image"));
    let dev_dyn: Arc<dyn BlockDevice> = dev.clone();
    let fs = Filesystem::mount(dev_dyn.clone()).expect("mount");
    Some((dev_dyn, fs))
}

fn read_inode_raw_bytes(fs: &Filesystem, ino: u32) -> Vec<u8> {
    let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino).expect("locate");
    let block_data = fs.read_block(block).expect("read block");
    let inode_size = fs.sb.inode_size as usize;
    let off = offset as usize;
    block_data[off..off + inode_size].to_vec()
}

fn inode_reader(
    fs: &Filesystem,
) -> impl FnMut(u32) -> Result<Inode> + '_ {
    move |ino: u32| -> Result<Inode> {
        let (block, offset) = bgd::locate_inode(&fs.sb, &fs.groups, ino)?;
        let block_data = fs.read_block(block)?;
        let inode_size = fs.sb.inode_size as usize;
        let off = offset as usize;
        Inode::parse(&block_data[off..off + inode_size])
    }
}

fn resolve(dev: &dyn BlockDevice, fs: &Filesystem, p: &str) -> u32 {
    let mut reader = inode_reader(fs);
    path::lookup(dev, &fs.sb, &mut reader, p).unwrap_or_else(|e| panic!("resolve {p}: {e}"))
}

fn read_parsed(fs: &Filesystem, ino: u32) -> Inode {
    let raw = read_inode_raw_bytes(fs, ino);
    Inode::parse(&raw).expect("parse")
}

#[test]
fn mode_only_file_has_three_short_entries() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    let ino = resolve(dev.as_ref(), &fs, "/mode_only.txt");
    let inode = read_parsed(&fs, ino);
    let raw = read_inode_raw_bytes(&fs, ino);

    let entries = acl::read(
        dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        AclKind::Access,
    )
    .expect("read acl");

    // A minimal mode-mapped ACL (u/g/o only) may be omitted by the kernel entirely
    // since it's derivable from st_mode. Tolerate both "no xattr" and "three entries".
    let Some(entries) = entries else {
        eprintln!("note: kernel stored mode-only ACL inline in st_mode (no xattr)");
        return;
    };
    assert_eq!(entries.len(), 3, "expected 3 entries, got {entries:?}");
    assert_eq!(entries[0].tag, AclTag::UserObj);
    assert_eq!(entries[0].perm, ACL_READ | ACL_WRITE | ACL_EXECUTE);
    assert_eq!(entries[1].tag, AclTag::GroupObj);
    assert_eq!(entries[1].perm, ACL_READ | ACL_EXECUTE);
    assert_eq!(entries[2].tag, AclTag::Other);
    assert_eq!(entries[2].perm, ACL_READ);
}

#[test]
fn named_user_and_group_entries_present() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    let ino = resolve(dev.as_ref(), &fs, "/named.txt");
    let inode = read_parsed(&fs, ino);
    let raw = read_inode_raw_bytes(&fs, ino);

    let entries = acl::read(
        dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        AclKind::Access,
    )
    .expect("read acl")
    .expect("/named.txt must have ACL xattr");

    let user_1000 = entries
        .iter()
        .find(|e| e.tag == AclTag::User && e.id == Some(1000))
        .expect("USER entry for uid 1000");
    assert_eq!(user_1000.perm, ACL_READ | ACL_WRITE);

    let group_2000 = entries
        .iter()
        .find(|e| e.tag == AclTag::Group && e.id == Some(2000))
        .expect("GROUP entry for gid 2000");
    assert_eq!(group_2000.perm, ACL_READ);

    let mask = entries.iter().find(|e| e.tag == AclTag::Mask).expect("MASK entry");
    assert_eq!(mask.perm, ACL_READ | ACL_WRITE | ACL_EXECUTE);
}

#[test]
fn directory_has_access_and_default_acl() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    let ino = resolve(dev.as_ref(), &fs, "/acl_dir");
    let inode = read_parsed(&fs, ino);
    let raw = read_inode_raw_bytes(&fs, ino);

    let access = acl::read(
        dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        AclKind::Access,
    )
    .expect("read access acl");
    // Kernel may omit minimal access ACL, so don't assert presence.
    if let Some(entries) = access {
        assert!(entries.iter().any(|e| e.tag == AclTag::UserObj));
    }

    let default = acl::read(
        dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
        AclKind::Default,
    )
    .expect("read default acl")
    .expect("/acl_dir must have default ACL");

    // Default ACL we set: d:u::rwx d:g::r-x d:o::---
    let user_obj = default
        .iter()
        .find(|e| e.tag == AclTag::UserObj)
        .expect("default UserObj");
    assert_eq!(user_obj.perm, ACL_READ | ACL_WRITE | ACL_EXECUTE);
    let group_obj = default
        .iter()
        .find(|e| e.tag == AclTag::GroupObj)
        .expect("default GroupObj");
    assert_eq!(group_obj.perm, ACL_READ | ACL_EXECUTE);
    let other = default
        .iter()
        .find(|e| e.tag == AclTag::Other)
        .expect("default Other");
    assert_eq!(other.perm, 0);
}

#[test]
fn plain_file_has_no_acl() {
    let Some((dev, fs)) = open_or_skip() else { return; };
    let ino = resolve(dev.as_ref(), &fs, "/plain.txt");
    let inode = read_parsed(&fs, ino);
    let raw = read_inode_raw_bytes(&fs, ino);

    for kind in [AclKind::Access, AclKind::Default] {
        let entries = acl::read(
            dev.as_ref(),
            &inode,
            &raw,
            fs.sb.inode_size,
            fs.sb.block_size(),
            kind,
        )
        .expect("read acl");
        assert!(entries.is_none(), "/plain.txt {kind:?} should be absent: {entries:?}");
    }

    // Also double-check via xattr listing that the two system.posix_acl_* names
    // really aren't present.
    let all = xattr::read_all(
        dev.as_ref(),
        &inode,
        &raw,
        fs.sb.inode_size,
        fs.sb.block_size(),
    )
    .expect("read xattrs");
    assert!(
        !all.iter().any(|e| e.name.starts_with("system.posix_acl_")),
        "unexpected ACL xattr: {all:?}"
    );
}
