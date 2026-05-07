//! Integration tests for the fsck repair pass.
//!
//! Three scenarios:
//! 1. `repair_fixes_duplicate_dirent_for_dir_inode` — fabricate the
//!    pre-fix `apply_mkdir` corruption shape (two dirents in root point
//!    at the same directory inode), run audit_with_repair, and assert
//!    the alias is gone, link count is sane, and a follow-up audit is
//!    clean.
//! 2. `audit_without_repair_leaves_corruption_intact` — same setup,
//!    `repair == false`: anomaly is reported, disk bytes are unchanged,
//!    and the next audit re-reports the same anomaly.
//! 3. `repair_fixes_link_count_drift` — clobber a regular file's
//!    `i_links_count` to a wrong-but-non-zero value, run repair, assert
//!    the count is rewritten to the observed value.
//!
//! Corruption is fabricated by reading the live root-dir data block,
//! patching the dirent header bytes in-place via the device's raw
//! `write_at`, then re-mounting so the audit sees the patched state.
//! This is hacky-by-design — production code never lays down dirents
//! this way.

use fs_ext4::block_io::FileDevice;
use fs_ext4::dir::{DirBlockIter, DirEntry, DirEntryType};
use fs_ext4::extent;
use fs_ext4::features;
use fs_ext4::fs::Filesystem;
use fs_ext4::fsck::{self, Anomaly};
use fs_ext4::inode::Inode;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

fn image_path(name: &str) -> String {
    format!("{}/test-disks/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn copy_to_tmp(name: &str, slot: &str) -> Option<String> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let src = image_path(name);
    if !std::path::Path::new(&src).exists() {
        return None;
    }
    let dst = format!(
        "/tmp/fs_ext4_fsck_repair_{}_{slot}_{n}.img",
        std::process::id()
    );
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// Read every entry in a directory inode's data blocks. Used by the
/// tests to verify post-repair state without going through the lookup
/// API (we want to see exactly how many dirents survive, including
/// any tombstones).
fn read_dir_entries(fs: &Filesystem, dir_ino: u32) -> Vec<DirEntry> {
    let (inode, _) = fs.read_inode_verified(dir_ino).expect("read dir inode");
    let has_ft = fs.sb.feature_incompat & features::Incompat::FILETYPE.bits() != 0;
    let bs = fs.sb.block_size();
    let total = inode.size.div_ceil(bs as u64);
    let mut out = Vec::new();
    let mut buf = vec![0u8; bs as usize];
    for logical in 0..total {
        let Some(phys) =
            extent::map_logical(&inode.block, fs.dev.as_ref(), bs, logical).expect("map")
        else {
            continue;
        };
        fs.dev.read_at(phys * bs as u64, &mut buf).expect("read");
        for entry in DirBlockIter::new(&buf, has_ft).flatten() {
            if entry.inode != 0 {
                out.push(entry);
            }
        }
    }
    out
}

/// Locate the byte offset of a dirent matching `name` inside the
/// directory inode's first physical block. Returns
/// (physical_block_number, offset_within_block, current_inode_field).
/// Caller patches the inode field to fabricate the duplicate state.
fn find_dirent_slot(fs: &Filesystem, dir_ino: u32, name: &[u8]) -> (u64, usize, u32) {
    let (inode, _) = fs.read_inode_verified(dir_ino).expect("read dir inode");
    let has_ft = fs.sb.feature_incompat & features::Incompat::FILETYPE.bits() != 0;
    let bs = fs.sb.block_size();
    let total = inode.size.div_ceil(bs as u64);
    let mut buf = vec![0u8; bs as usize];
    for logical in 0..total {
        let Some(phys) =
            extent::map_logical(&inode.block, fs.dev.as_ref(), bs, logical).expect("map")
        else {
            continue;
        };
        fs.dev.read_at(phys * bs as u64, &mut buf).expect("read");
        let mut off = 0usize;
        while off + 8 <= buf.len() {
            let cur_inode = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
            let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap()) as usize;
            if rec_len < 8 || !rec_len.is_multiple_of(4) || off + rec_len > buf.len() {
                break;
            }
            if cur_inode != 0 {
                let cur_name_lo = buf[off + 6];
                let cur_type_or_hi = buf[off + 7];
                let cur_name_len = if has_ft {
                    cur_name_lo as usize
                } else {
                    ((cur_type_or_hi as usize) << 8) | cur_name_lo as usize
                };
                if 8 + cur_name_len <= rec_len && &buf[off + 8..off + 8 + cur_name_len] == name {
                    return (phys, off, cur_inode);
                }
            }
            off += rec_len;
        }
    }
    panic!("dirent {:?} not found in dir ino {}", name, dir_ino);
}

/// Write the four bytes at (block, offset) without going through the
/// filesystem's BlockBuffer or journal — bypasses the buffer cache by
/// re-opening the underlying file at the OS level. Used to fabricate
/// raw corruption that subsequent mounts/audits will then see.
fn poke_inode_field_raw(
    image_path: &str,
    block_size: u32,
    phys_block: u64,
    off: usize,
    new_inode: u32,
) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_path)
        .expect("open raw");
    f.seek(SeekFrom::Start(phys_block * block_size as u64 + off as u64))
        .expect("seek");
    f.write_all(&new_inode.to_le_bytes()).expect("write");
    // CRC tail will be wrong after the patch; re-mount paths that
    // verify dir csums will reject the block. The audit walker
    // tolerates parse errors, so the duplicate is still surfaced —
    // but to keep the audit clean *after* the targeted fabrication
    // we rewrite the tail crc too. Done in `repair_dir_csum_after_poke`
    // by the caller when needed.
}

/// Recompute and write back the dir-csum tail for `phys_block` so the
/// patched block re-validates. Mirrors the formula in
/// `Checksummer::verify_dir_entry_tail`: crc32c covers everything from
/// byte 0 up to (block_size - 12), seeded with crate seed → ino → gen.
fn fix_dir_csum_after_poke(image_path: &str, fs: &Filesystem, dir_ino: u32, phys_block: u64) {
    use std::io::{Seek, SeekFrom, Write};
    if !fs.csum.enabled {
        return;
    }
    let bs = fs.sb.block_size() as usize;
    // Read the latest block bytes through the live fs (which is
    // post-poke since this fn runs after the fs has been re-opened).
    let block = fs.read_block(phys_block).expect("read block");
    if !fs_ext4::dir::has_csum_tail(&block) {
        return;
    }
    let (inode, _) = fs.read_inode_verified(dir_ino).expect("read inode");
    let mut c = fs_ext4::checksum::linux_crc32c(fs.csum.seed, &dir_ino.to_le_bytes());
    c = fs_ext4::checksum::linux_crc32c(c, &inode.generation.to_le_bytes());
    c = fs_ext4::checksum::linux_crc32c(c, &block[..bs - 12]);
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_path)
        .expect("open raw");
    f.seek(SeekFrom::Start(phys_block * bs as u64 + (bs as u64 - 4)))
        .expect("seek");
    f.write_all(&c.to_le_bytes()).expect("write csum");
}

/// Set up a corrupted image that has duplicate dirents pointing at
/// one directory inode. Returns (image_path, kept_ino, alias_names).
fn make_duplicate_dir_inode_corruption() -> Option<(String, u32, Vec<String>)> {
    let path = copy_to_tmp("ext4-basic.img", "dup")?;

    // Create three subdirs so we have real targets to alias around.
    let alias_target_ino;
    let kept_name;
    let alias_names = vec!["alias_a".to_string(), "alias_b".to_string()];
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let kept_ino = fs.apply_mkdir("/keeper", 0o755).expect("mkdir keeper");
        // Two extra subdirs we'll later turn into aliases of /keeper.
        for n in &alias_names {
            let p = format!("/{n}");
            let _ = fs.apply_mkdir(&p, 0o755).expect("mkdir alias placeholder");
        }
        alias_target_ino = kept_ino;
        kept_name = "keeper".to_string();
        // fs drop here flushes journaled writes so the file on disk
        // has the post-mkdir state visible to a raw re-open.
    }

    // Re-open via mount so we can read live (correct) dir layout;
    // record block + offset of the placeholder dirents.
    let (slot_a, slot_b);
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 2");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 2");
        slot_a = find_dirent_slot(
            &fs,
            fs_ext4::path::EXT4_ROOT_INODE,
            alias_names[0].as_bytes(),
        );
        slot_b = find_dirent_slot(
            &fs,
            fs_ext4::path::EXT4_ROOT_INODE,
            alias_names[1].as_bytes(),
        );
        // Drop fs so its CachedDevice doesn't hold stale bytes.
    }

    let bs = 4096u32;
    // Patch both dirents so they point at /keeper's inode. Now the
    // root dir has THREE dirents (keeper + alias_a + alias_b) all
    // pointing at the same inode — exactly the pre-fix mkdir bug.
    poke_inode_field_raw(&path, bs, slot_a.0, slot_a.1, alias_target_ino);
    poke_inode_field_raw(&path, bs, slot_b.0, slot_b.1, alias_target_ino);

    // Recompute the root dir block's csum tail so the audit's mount
    // path doesn't reject the block as bad-csum (which would mask
    // the duplicate-detection signal we want to test).
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 3");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 3");
        // slot_a / slot_b live in the same root data block in
        // practice (small dir, single block). Fix once.
        fix_dir_csum_after_poke(&path, &fs, fs_ext4::path::EXT4_ROOT_INODE, slot_a.0);
        if slot_b.0 != slot_a.0 {
            fix_dir_csum_after_poke(&path, &fs, fs_ext4::path::EXT4_ROOT_INODE, slot_b.0);
        }
    }

    let _ = kept_name;
    Some((path, alias_target_ino, alias_names))
}

#[test]
fn repair_fixes_duplicate_dirent_for_dir_inode() {
    let Some((path, kept_ino, alias_names)) = make_duplicate_dir_inode_corruption() else {
        eprintln!("skip: ext4-basic.img not present");
        return;
    };

    // Sanity: pre-repair audit must see the duplicate.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let report = fs.audit(u32::MAX, u32::MAX).expect("audit");
        assert!(
            report.anomalies.iter().any(
                |a| matches!(a, Anomaly::DuplicateDirentForDirInode { ino, .. } if *ino == kept_ino)
            ),
            "expected DuplicateDirentForDirInode for ino {kept_ino}, got {:?}",
            report.anomalies
        );
    }

    // Run repair.
    let repaired = {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let report = fsck::audit_with_repair(&fs, u32::MAX, u32::MAX, |_, _, _| {}, |_| {}, true)
            .expect("audit_with_repair");
        // Two duplicate dirents removed = repaired_count == 2.
        report.repaired_count
    };
    assert!(
        repaired >= 2,
        "expected at least 2 duplicate dirents repaired, got {repaired}"
    );

    // Re-mount and verify root has only one dirent for kept_ino, and
    // a fresh audit comes back clean for that variant. Repair's
    // documented contract is "keep dirents[0] (sorted), remove the
    // rest" — with alphabetic sorting that means the surviving name
    // is whichever sorts first among {keeper, alias_a, alias_b}.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let entries = read_dir_entries(&fs, fs_ext4::path::EXT4_ROOT_INODE);
        let count_pointing_at_kept = entries.iter().filter(|e| e.inode == kept_ino).count();
        assert_eq!(
            count_pointing_at_kept, 1,
            "post-repair root must have exactly one dirent for ino {kept_ino}"
        );

        // Exactly one of the original three names must survive; the
        // other two must be gone. (Which one depends on sort order;
        // we don't pin that here so the test remains stable if the
        // sort tiebreaker policy changes.)
        let candidate_names: Vec<String> = std::iter::once("keeper".to_string())
            .chain(alias_names.iter().cloned())
            .collect();
        let surviving: Vec<&[u8]> = entries
            .iter()
            .filter(|e| e.inode == kept_ino)
            .map(|e| e.name.as_slice())
            .collect();
        assert_eq!(surviving.len(), 1);
        assert!(
            candidate_names.iter().any(|n| n.as_bytes() == surviving[0]),
            "surviving dirent name {:?} not one of the candidates {:?}",
            String::from_utf8_lossy(surviving[0]),
            candidate_names
        );
        let surviving_name = String::from_utf8_lossy(surviving[0]).into_owned();
        let dropped_count = candidate_names
            .iter()
            .filter(|n| **n != surviving_name)
            .filter(|n| entries.iter().any(|e| e.name == n.as_bytes()))
            .count();
        assert_eq!(
            dropped_count, 0,
            "all duplicate-named dirents except {surviving_name:?} must be removed"
        );

        let report = fs.audit(u32::MAX, u32::MAX).expect("post audit");
        assert!(
            !report
                .anomalies
                .iter()
                .any(|a| matches!(a, Anomaly::DuplicateDirentForDirInode { .. })),
            "duplicate-dir-inode anomalies must not survive a successful repair, got {:?}",
            report.anomalies
        );
    }
}

#[test]
fn audit_without_repair_leaves_corruption_intact() {
    let Some((path, kept_ino, _)) = make_duplicate_dir_inode_corruption() else {
        eprintln!("skip: ext4-basic.img not present");
        return;
    };

    // First pass: repair = false. Anomaly reported, no mutation.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let report = fsck::audit_with_repair(&fs, u32::MAX, u32::MAX, |_, _, _| {}, |_| {}, false)
            .expect("audit_with_repair (read-only)");
        assert_eq!(
            report.repaired_count, 0,
            "repair=false must never bump repaired_count"
        );
        assert!(
            report.anomalies.iter().any(
                |a| matches!(a, Anomaly::DuplicateDirentForDirInode { ino, .. } if *ino == kept_ino)
            ),
            "the anomaly must still be reported"
        );
    }

    // Second pass: re-mount, audit. The anomaly persists because the
    // first pass never wrote anything back.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let report = fs.audit(u32::MAX, u32::MAX).expect("audit");
        assert!(
            report.anomalies.iter().any(
                |a| matches!(a, Anomaly::DuplicateDirentForDirInode { ino, .. } if *ino == kept_ino)
            ),
            "anomaly must persist across remount when repair was disabled"
        );
    }
}

#[test]
fn repair_fixes_link_count_drift() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "linkfix") else {
        eprintln!("skip: ext4-basic.img not present");
        return;
    };

    // Create one regular file so we have an inode whose observed
    // link count (== 1) is stable. We then clobber its on-disk
    // i_links_count to 7 and ask repair to bring it back.
    let target_ino;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        target_ino = fs.apply_create("/linkdrift.txt", 0o644).expect("create");
        // Fs drop flushes the journaled inode write.
    }

    // Patch i_links_count = 7 via the safe API path: we mount once
    // to learn the inode's location, then write through the live fs
    // (it journals) — but that would also auto-fix the csum. Easier:
    // poke raw bytes for the count and let `read_inode_verified`
    // surface the bad-csum if the image had csums on. ext4-basic.img
    // does have metadata_csum, so we patch via the live API: read,
    // modify, write_inode_raw (NOT journaled) — fine for a test
    // fixture since there are no concurrent writers.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let (inode, mut raw) = fs.read_inode_verified(target_ino).expect("read inode");
        raw[0x1A..0x1C].copy_from_slice(&7u16.to_le_bytes());
        // Recompute the inode csum so the audit can read the inode
        // back without tripping BadChecksum.
        if fs.csum.enabled {
            if let Some((lo, hi)) =
                fs.csum
                    .compute_inode_checksum(target_ino, inode.generation, &raw)
            {
                raw[0x7C..0x7E].copy_from_slice(&lo.to_le_bytes());
                if raw.len() >= 0x84 {
                    raw[0x82..0x84].copy_from_slice(&hi.to_le_bytes());
                }
            }
        }
        fs.write_inode_raw(target_ino, &raw)
            .expect("write_inode_raw");
        let _ = inode;
        // Drop fs.
    }

    // Run repair.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let pre = fs.audit(u32::MAX, u32::MAX).expect("pre audit");
        assert!(
            pre.anomalies.iter().any(|a| matches!(
                a,
                Anomaly::LinkCountTooHigh { ino, stored: 7, .. } if *ino == target_ino
            )),
            "expected LinkCountTooHigh on target ino, got {:?}",
            pre.anomalies
        );

        let report = fsck::audit_with_repair(&fs, u32::MAX, u32::MAX, |_, _, _| {}, |_| {}, true)
            .expect("repair");
        assert!(
            report.repaired_count >= 1,
            "expected at least one link-count repair, got {}",
            report.repaired_count
        );
    }

    // Re-mount, audit, expect clean for that inode.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        let (inode, _) = fs.read_inode_verified(target_ino).expect("read inode");
        assert_eq!(
            inode.links_count, 1,
            "post-repair links_count should be 1, got {}",
            inode.links_count
        );
        let report = fs.audit(u32::MAX, u32::MAX).expect("audit");
        assert!(
            !report.anomalies.iter().any(|a| matches!(
                a,
                Anomaly::LinkCountTooHigh { ino, .. } | Anomaly::LinkCountTooLow { ino, .. }
                    if *ino == target_ino
            )),
            "link-count anomaly for ino {target_ino} must be gone, got {:?}",
            report.anomalies
        );
    }
}

/// Fabricate the WrongDotDot scenario: create /subdir, then patch
/// subdir's ".." dirent to claim a fake parent inode. Returns
/// (image_path, subdir_ino, fake_parent_ino).
fn make_wrong_dotdot_corruption() -> Option<(String, u32, u32)> {
    let path = copy_to_tmp("ext4-basic.img", "wrongdotdot")?;

    let subdir_ino;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        subdir_ino = fs
            .apply_mkdir("/wrongdotdot_test", 0o755)
            .expect("mkdir test dir");
        // fs drop flushes journaled writes.
    }

    // Find ".." dirent slot in subdir.
    let dotdot_slot;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 2");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 2");
        dotdot_slot = find_dirent_slot(&fs, subdir_ino, b"..");
        // Sanity: ".." should currently point at root before we patch.
        assert_eq!(
            dotdot_slot.2,
            fs_ext4::path::EXT4_ROOT_INODE,
            "fresh subdir's .. should point at root"
        );
    }

    // Patch ".." to claim a fake parent (an inode that doesn't exist
    // in this image). The audit's WrongDotDot detection compares the
    // claim against the walker's actual_parent map, so any value
    // other than 2 will trigger.
    let fake_parent: u32 = 99;
    let bs = 4096u32;
    poke_inode_field_raw(&path, bs, dotdot_slot.0, dotdot_slot.1, fake_parent);

    // Re-csum the patched block so the audit's mount path doesn't
    // reject it as bad-csum (which would mask the wrong-dotdot signal).
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 3");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 3");
        fix_dir_csum_after_poke(&path, &fs, subdir_ino, dotdot_slot.0);
    }

    Some((path, subdir_ino, fake_parent))
}

#[test]
fn repair_fixes_wrong_dotdot() {
    let Some((path, subdir_ino, fake_parent)) = make_wrong_dotdot_corruption() else {
        eprintln!("skip: ext4-basic.img not present");
        return;
    };

    // Phase 1: audit-only confirms WrongDotDot is detected.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw audit");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount audit");
        let report = fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit");
        let detected = report.anomalies.iter().any(|a| {
            matches!(a,
                Anomaly::WrongDotDot { dir_ino, claims, actual_parent }
                    if *dir_ino == subdir_ino
                        && *claims == fake_parent
                        && *actual_parent == fs_ext4::path::EXT4_ROOT_INODE
            )
        });
        assert!(
            detected,
            "expected WrongDotDot for subdir {subdir_ino} (got {:#?})",
            report.anomalies
        );
    }

    // Phase 2: repair pass writes the fix.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw repair");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount repair");
        let report = fsck::audit_with_repair(&fs, u32::MAX, u32::MAX, |_, _, _| {}, |_| {}, true)
            .expect("repair");
        assert!(
            report.repaired_count >= 1,
            "expected at least one repair, got {}",
            report.repaired_count
        );
        // Note: we don't assert `anomalies_count == 0` here. Single-
        // pass repair can leave second-order anomalies — fixing the
        // ".." dirent shifts which inode the subdir's reference
        // credits in the next walk, which can knock a previously-
        // repaired link count one off. Reconciling that requires a
        // convergence loop in `audit_with_repair`, which is wider
        // scope than this change. The Phase-3 standalone re-audit
        // below confirms the specific anomaly THIS repair targets
        // is gone, which is the contract for the repair.
    }

    // Phase 3: standalone re-audit confirms the fix landed on disk
    // (not just in a cached buffer).
    {
        let dev = FileDevice::open_rw(&path).expect("open rw verify");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount verify");
        let report = fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit verify");
        let still_wrong = report
            .anomalies
            .iter()
            .any(|a| matches!(a, Anomaly::WrongDotDot { dir_ino, .. } if *dir_ino == subdir_ino));
        assert!(
            !still_wrong,
            "WrongDotDot still present after repair: {:#?}",
            report.anomalies
        );

        // Also verify the on-disk dirent now reads back as root.
        let (_blk, _off, current_inode) = find_dirent_slot(&fs, subdir_ino, b"..");
        assert_eq!(
            current_inode,
            fs_ext4::path::EXT4_ROOT_INODE,
            "subdir's .. should point at root after repair"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// Patch a single byte at (phys_block, off). Used to flip a dirent's
/// file_type byte (offset 7 of the dirent record) to fabricate a
/// BogusEntry without disturbing the rest of the layout.
fn poke_byte_raw(image_path: &str, block_size: u32, phys_block: u64, off: usize, new_byte: u8) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_path)
        .expect("open raw");
    f.seek(SeekFrom::Start(phys_block * block_size as u64 + off as u64))
        .expect("seek");
    f.write_all(&[new_byte]).expect("write byte");
}

/// Fabricate a BogusEntry: create /foo as a regular file, then
/// rewrite its dirent's file_type byte (offset+7) to claim Directory
/// (=2). Returns (image_path, file_ino) — the file_ino is the child
/// the audit will report in BogusEntry.child_ino.
fn make_bogus_entry_corruption() -> Option<(String, u32)> {
    let path = copy_to_tmp("ext4-basic.img", "bogus")?;

    let file_ino;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount");
        file_ino = fs
            .apply_create("/bogus_test", 0o644)
            .expect("create regular file");
    }

    // Find the dirent for /bogus_test in root and flip its file_type
    // byte from 1 (REG_FILE) to 2 (Directory).
    let slot;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 2");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 2");
        slot = find_dirent_slot(&fs, fs_ext4::path::EXT4_ROOT_INODE, b"bogus_test");
        assert_eq!(slot.2, file_ino, "dirent inode should match the new file");
    }
    let bs = 4096u32;
    poke_byte_raw(&path, bs, slot.0, slot.1 + 7, DirEntryType::Directory as u8);

    // Re-csum the patched root-dir block so the audit accepts it.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw 3");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount 3");
        fix_dir_csum_after_poke(&path, &fs, fs_ext4::path::EXT4_ROOT_INODE, slot.0);
    }

    Some((path, file_ino))
}

#[test]
fn repair_fixes_bogus_entry() {
    let Some((path, file_ino)) = make_bogus_entry_corruption() else {
        eprintln!("skip: ext4-basic.img not present");
        return;
    };

    // Phase 1: audit detects BogusEntry with our injected child_ino.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw audit");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount audit");
        let report = fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit");
        let detected = report.anomalies.iter().any(|a| {
            matches!(a,
                Anomaly::BogusEntry { parent_ino, child_ino }
                    if *parent_ino == fs_ext4::path::EXT4_ROOT_INODE && *child_ino == file_ino
            )
        });
        assert!(
            detected,
            "expected BogusEntry for child {file_ino} (got {:#?})",
            report.anomalies
        );
    }

    // Phase 2: repair rewrites the file_type byte.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw repair");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount repair");
        let report = fsck::audit_with_repair(&fs, u32::MAX, u32::MAX, |_, _, _| {}, |_| {}, true)
            .expect("repair");
        assert!(
            report.repaired_count >= 1,
            "expected at least one repair, got {} (initial {})",
            report.repaired_count,
            report.initial_anomalies_count
        );
    }

    // Phase 3: standalone re-audit confirms BogusEntry is gone.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw verify");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount verify");
        let report = fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit verify");
        let still_bogus = report
            .anomalies
            .iter()
            .any(|a| matches!(a, Anomaly::BogusEntry { child_ino, .. } if *child_ino == file_ino));
        assert!(
            !still_bogus,
            "BogusEntry still present after repair: {:#?}",
            report.anomalies
        );

        // The dirent's file_type byte should now read back as RegFile.
        let entries = read_dir_entries(&fs, fs_ext4::path::EXT4_ROOT_INODE);
        let foo = entries
            .iter()
            .find(|e| e.name == b"bogus_test")
            .expect("bogus_test entry survived repair");
        assert_eq!(
            foo.file_type,
            DirEntryType::RegFile,
            "file_type byte should be RegFile after repair"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// Patch the superblock's `s_free_blocks_count` (lo half) at file
/// offset SUPERBLOCK_OFFSET + 0x0C, then recompute the SB checksum
/// when the on-disk image has it enabled. Mirrors the formula in
/// `Filesystem::patch_sb_counters`. The image's checksum-enabled bit
/// is at SB offset 0x6C (s_checksum_type — non-zero means enabled).
fn poke_sb_free_blocks_count_lo(image_path: &str, new_lo: u32) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_path)
        .expect("open raw");
    // Read the whole 1024-byte SB block so we can recompute the CRC.
    let mut sb_raw = vec![0u8; 1024];
    f.seek(SeekFrom::Start(1024)).expect("seek sb");
    f.read_exact(&mut sb_raw).expect("read sb");

    // Patch the lo half.
    sb_raw[0x0C..0x10].copy_from_slice(&new_lo.to_le_bytes());

    // Detect csum-enabled. ext4's `s_checksum_type` (offset 0x175,
    // u8) is non-zero when metadata_csum is on. Mirrors `Csum::enabled`
    // logic in src/superblock.rs without depending on the parsed sb.
    let csum_type = sb_raw[0x175];
    if csum_type != 0 {
        let csum = fs_ext4::checksum::linux_crc32c(!0, &sb_raw[..0x3FC]);
        sb_raw[0x3FC..0x400].copy_from_slice(&csum.to_le_bytes());
    }

    f.seek(SeekFrom::Start(1024)).expect("seek sb 2");
    f.write_all(&sb_raw).expect("write sb");
}

#[test]
fn repair_fixes_superblock_free_count_drift() {
    let Some(path) = copy_to_tmp("ext4-basic.img", "freecount") else {
        eprintln!("skip: ext4-basic.img not present");
        return;
    };

    // Read the current SB free_blocks_count, then clobber with a
    // bogus value so the bitmap-derived sum disagrees.
    let original_lo: u32;
    {
        let dev = FileDevice::open_rw(&path).expect("open rw probe");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount probe");
        original_lo = (fs.sb.free_blocks_count & 0xFFFF_FFFF) as u32;
    }
    let bogus_lo = original_lo.wrapping_add(13);
    poke_sb_free_blocks_count_lo(&path, bogus_lo);

    // Phase 1: audit detects the drift.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw audit");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount audit");
        let report = fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit");
        let detected = report.anomalies.iter().any(|a| {
            matches!(
                a,
                Anomaly::SuperblockFreeCountDrift {
                    stored_blocks,
                    observed_blocks,
                    ..
                } if *stored_blocks != *observed_blocks
            )
        });
        assert!(
            detected,
            "expected SuperblockFreeCountDrift (got {:#?})",
            report.anomalies
        );
    }

    // Phase 2: repair patches the SB.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw repair");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount repair");
        let report = fsck::audit_with_repair(&fs, u32::MAX, u32::MAX, |_, _, _| {}, |_| {}, true)
            .expect("repair");
        assert!(
            report.repaired_count >= 1,
            "expected at least one repair, got {}",
            report.repaired_count
        );
    }

    // Phase 3: re-audit confirms drift is gone.
    {
        let dev = FileDevice::open_rw(&path).expect("open rw verify");
        let fs = Filesystem::mount(Arc::new(dev)).expect("mount verify");
        let report = fsck::audit(&fs, u32::MAX, u32::MAX).expect("audit verify");
        let still_drifted = report
            .anomalies
            .iter()
            .any(|a| matches!(a, Anomaly::SuperblockFreeCountDrift { .. }));
        assert!(
            !still_drifted,
            "SuperblockFreeCountDrift still present: {:#?}",
            report.anomalies
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// Use `Inode` import so the `use` line stays referenced when only
/// some tests are compiled in (avoids a dead-code warning under
/// `cargo check --tests` with feature flags off).
#[allow(dead_code)]
fn _force_inode_use(i: &Inode) -> u16 {
    i.links_count
}

/// Same for DirEntryType — referenced indirectly through DirEntry.
#[allow(dead_code)]
fn _force_dirent_type_use(t: DirEntryType) -> u32 {
    t as u32
}
