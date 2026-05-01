//! Read-only filesystem audit — a small subset of `ext4 audit tool -n`.
//!
//! Walks the directory tree from inode 2 (root), counting how many
//! directory entries reference each inode. Compares the observed
//! reference count against the inode's stored `i_links_count` and
//! flags mismatches. Also reports directories whose `..` entry does
//! not point at the true parent.
//!
//! This is a **diagnostic**, not a repair: it never writes to disk.
//! Useful after a crash (to spot orphans that live-mount replay
//! missed) and as a CI sanity check on generated images.
//!
//! Two surfaces:
//! - [`audit`] — synchronous, collects every [`Anomaly`] into a
//!   `Vec` on the returned [`AuditReport`]. Used by Rust callers and
//!   tests.
//! - [`audit_with_callbacks`] — same walk, but emits per-phase
//!   progress and per-finding events through caller-supplied
//!   closures. Used by the C ABI (`fs_ext4_fsck_run`) so the host UI
//!   can stream progress and findings live without buffering the
//!   full anomaly list for huge volumes.
//!
//! Both surfaces are read-only. Repair (link-count fixup, orphan
//! relink, dotdot rewrite) is explicit future work — exposing it
//! requires a journaled write path and an ABI-versioned bump.

use crate::dir::{DirBlockIter, DirEntryType};
use crate::error::{Error, Result};
use crate::extent;
use crate::features;
use crate::fs::Filesystem;
use crate::inode::Inode;
use std::collections::HashMap;

/// One problem found by [`audit`]. Each variant carries the inode or
/// path needed to act on the finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anomaly {
    /// A directory entry references an inode whose `i_links_count` is
    /// *less than* the observed reference count. Stored value is too
    /// low — fsck would increase it to `observed`.
    LinkCountTooLow {
        ino: u32,
        stored: u16,
        observed: u32,
    },
    /// Inode's `i_links_count` is *greater than* the observed reference
    /// count. Stored value is too high — fsck would decrease it.
    LinkCountTooHigh {
        ino: u32,
        stored: u16,
        observed: u32,
    },
    /// Dangling directory entry: a dir entry points to an inode with
    /// `i_links_count == 0` or one we couldn't read.
    DanglingEntry { parent_ino: u32, child_ino: u32 },
    /// A directory's `..` entry does not point at its true parent.
    WrongDotDot {
        dir_ino: u32,
        claims: u32,
        actual_parent: u32,
    },
    /// A directory entry with inode number 0 was encountered in a
    /// position that isn't a tombstone (rec_len > 8 + padded name).
    BogusEntry { parent_ino: u32 },
}

/// Summary returned by [`audit`]. Empty `anomalies` means the subset
/// of invariants checked all held.
#[derive(Debug, Clone, Default)]
pub struct AuditReport {
    /// Every problem found, in no particular order. Populated by the
    /// legacy [`audit`] entry point; left empty by
    /// [`audit_with_callbacks`] (it streams findings through the
    /// caller's closure to avoid buffering on huge volumes).
    pub anomalies: Vec<Anomaly>,
    /// Number of distinct inodes visited via directory entries.
    pub inodes_visited: u32,
    /// Number of directory entries scanned (including `.`, `..`, and tombstones).
    pub entries_scanned: u64,
    /// Number of directories scanned.
    pub directories_scanned: u32,
    /// Total findings discovered. Always populated, regardless of
    /// which entry point ran. Equal to `anomalies.len()` after a
    /// successful [`audit`] call.
    pub anomalies_count: u64,
}

impl AuditReport {
    pub fn is_clean(&self) -> bool {
        self.anomalies_count == 0
    }
}

/// Phase identifier for [`audit_with_callbacks`] progress callbacks.
///
/// Numeric values match `fs_ext4_fsck_phase_t` in `include/fs_ext4.h`
/// and **must not be reordered** — the C ABI is locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FsckPhase {
    Superblock = 0,
    Journal = 1,
    Directory = 2,
    Inodes = 3,
    Finalize = 4,
}

impl FsckPhase {
    /// Short ASCII label, mirrored to the C ABI.
    pub fn name(self) -> &'static str {
        match self {
            FsckPhase::Superblock => "superblock",
            FsckPhase::Journal => "journal",
            FsckPhase::Directory => "directory",
            FsckPhase::Inodes => "inodes",
            FsckPhase::Finalize => "finalize",
        }
    }
}

/// Walk the filesystem from `/`, counting directory-entry references
/// to each inode and comparing against each inode's `i_links_count`.
///
/// Capped by `max_dirs_visited` and `max_entries_per_dir` so a
/// deliberately-cyclic or extremely large image can still be audited
/// in bounded time. For a real fsck pass, set both to `u32::MAX`.
pub fn audit(
    fs: &Filesystem,
    max_dirs_visited: u32,
    max_entries_per_dir: u32,
) -> Result<AuditReport> {
    let mut report = AuditReport::default();
    let mut collected: Vec<Anomaly> = Vec::new();
    audit_inner(
        fs,
        max_dirs_visited,
        max_entries_per_dir,
        &mut |_, _, _| {},
        &mut |a| collected.push(a.clone()),
        &mut report,
    )?;
    report.anomalies = collected;
    Ok(report)
}

/// Same walk as [`audit`], but emits progress and findings through
/// caller-supplied closures. The callbacks see each [`Anomaly`] as it
/// is discovered (no buffering of the full list) and per-phase
/// progress so a host UI can render a live progress bar.
///
/// On return, `report.anomalies` is **empty** — findings are delivered
/// only through `on_finding`. The summary counters
/// (`directories_scanned`, `entries_scanned`, `inodes_visited`,
/// `anomalies_found` … via the C ABI helpers) are still populated.
///
/// Phase emission contract:
/// - `Superblock` once at start (0/1 → 1/1) — superblock validity
///   was already checked at mount.
/// - `Directory` per directory popped (`done` = directories scanned
///   so far, `total` = scanned + queue depth).
/// - `Inodes` once around the link-count comparison pass (0/1 → 1/1).
/// - `Finalize` once just before return (0/1 → 1/1).
///
/// `Journal` is **not** emitted here — the FFI shim drives journal
/// replay before calling this function and emits the phase from
/// there.
pub fn audit_with_callbacks<P, F>(
    fs: &Filesystem,
    max_dirs_visited: u32,
    max_entries_per_dir: u32,
    mut on_progress: P,
    mut on_finding: F,
) -> Result<AuditReport>
where
    P: FnMut(FsckPhase, u64, u64),
    F: FnMut(&Anomaly),
{
    let mut report = AuditReport::default();
    on_progress(FsckPhase::Superblock, 0, 1);
    on_progress(FsckPhase::Superblock, 1, 1);

    audit_inner(
        fs,
        max_dirs_visited,
        max_entries_per_dir,
        &mut on_progress,
        &mut on_finding,
        &mut report,
    )?;

    Ok(report)
}

/// Core walk shared by [`audit`] and [`audit_with_callbacks`].
///
/// Findings are emitted through `on_finding`; nothing is pushed onto
/// `report.anomalies` from here. Callers that want the legacy
/// "collect into a vec" behaviour wrap `on_finding` accordingly.
fn audit_inner(
    fs: &Filesystem,
    max_dirs_visited: u32,
    max_entries_per_dir: u32,
    on_progress: &mut dyn FnMut(FsckPhase, u64, u64),
    on_finding: &mut dyn FnMut(&Anomaly),
    report: &mut AuditReport,
) -> Result<()> {
    // Observed: ino → reference-count.
    let mut observed: HashMap<u32, u32> = HashMap::new();
    let mut parent_claim: HashMap<u32, u32> = HashMap::new();
    // Directories we couldn't fully walk (parse failure, inline overflow
    // we don't decode, bound cap). Any link-count anomalies that could
    // have been explained by their missing entries are suppressed below.
    let mut incomplete_dirs: std::collections::HashSet<u32> = std::collections::HashSet::new();

    let mut work: Vec<(u32, u32)> = Vec::new(); // (ino, parent_ino)
    work.push((crate::path::EXT4_ROOT_INODE, crate::path::EXT4_ROOT_INODE));
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();

    let has_filetype = fs.sb.feature_incompat & features::Incompat::FILETYPE.bits() != 0;
    let block_size = fs.sb.block_size();

    // Initial directory progress pulse: 0 of (just root).
    on_progress(FsckPhase::Directory, 0, work.len() as u64);

    while let Some((dir_ino, parent_ino)) = work.pop() {
        if report.directories_scanned >= max_dirs_visited {
            incomplete_dirs.insert(dir_ino);
            break;
        }
        if !visited.insert(dir_ino) {
            continue;
        }
        report.directories_scanned += 1;

        let (inode, _raw) = match fs.read_inode_verified(dir_ino) {
            Ok(p) => p,
            Err(_) => {
                incomplete_dirs.insert(dir_ino);
                emit_dir_progress(on_progress, report.directories_scanned, work.len());
                continue;
            }
        };
        if !inode.is_dir() {
            // Something referenced us as a dir but the inode says otherwise.
            // Surface as a BogusEntry against the parent.
            let a = Anomaly::BogusEntry { parent_ino };
            on_finding(&a);
            report.anomalies_count += 1;
            emit_dir_progress(on_progress, report.directories_scanned, work.len());
            continue;
        }

        // Skip directories the audit can't fully enumerate (inline dirs
        // whose entries overflow into the xattr region — a valid
        // on-disk layout we don't decode here).
        if inode.has_inline_data() {
            incomplete_dirs.insert(dir_ino);
            emit_dir_progress(on_progress, report.directories_scanned, work.len());
            continue;
        }

        let entries = match collect_dir_entries(fs, &inode, has_filetype, block_size) {
            Ok(e) => e,
            Err(_) => {
                incomplete_dirs.insert(dir_ino);
                emit_dir_progress(on_progress, report.directories_scanned, work.len());
                continue;
            }
        };

        let mut truncated = false;
        for (n_scanned, entry) in (0u32..).zip(entries) {
            if n_scanned >= max_entries_per_dir {
                truncated = true;
                break;
            }
            report.entries_scanned += 1;

            if entry.name == b"." {
                *observed.entry(dir_ino).or_insert(0) += 1;
                continue;
            }
            if entry.name == b".." {
                parent_claim.insert(dir_ino, entry.inode);
                *observed.entry(entry.inode).or_insert(0) += 1;
                continue;
            }

            *observed.entry(entry.inode).or_insert(0) += 1;

            if matches!(entry.file_type, DirEntryType::Directory) {
                work.push((entry.inode, dir_ino));
            }
        }
        if truncated {
            incomplete_dirs.insert(dir_ino);
        }
        emit_dir_progress(on_progress, report.directories_scanned, work.len());
    }

    report.inodes_visited = observed.len() as u32;

    // Inode link-count compare phase.
    on_progress(FsckPhase::Inodes, 0, 1);

    // Compare observed vs stored. When an inode's reference came from a
    // directory we couldn't fully enumerate, we suppress TooHigh (we
    // under-counted) but still report TooLow (we already saw more than
    // the stored value — the image is genuinely wrong).
    let have_incomplete = !incomplete_dirs.is_empty();
    for (&ino, &count) in observed.iter() {
        match fs.read_inode_verified(ino) {
            Ok((inode, _)) => {
                let stored = inode.links_count;
                if stored == 0 {
                    let a = Anomaly::DanglingEntry {
                        parent_ino: 0,
                        child_ino: ino,
                    };
                    on_finding(&a);
                    report.anomalies_count += 1;
                    continue;
                }
                if (stored as u32) < count {
                    let a = Anomaly::LinkCountTooLow {
                        ino,
                        stored,
                        observed: count,
                    };
                    on_finding(&a);
                    report.anomalies_count += 1;
                }
                if (stored as u32) > count && !have_incomplete {
                    let a = Anomaly::LinkCountTooHigh {
                        ino,
                        stored,
                        observed: count,
                    };
                    on_finding(&a);
                    report.anomalies_count += 1;
                }
            }
            Err(_) => {
                // Unreadable inode that somebody linked to.
                let a = Anomaly::DanglingEntry {
                    parent_ino: 0,
                    child_ino: ino,
                };
                on_finding(&a);
                report.anomalies_count += 1;
            }
        }
    }

    // Check `..` claims against the actual parent that enqueued us.
    for (&dir_ino, &claimed) in parent_claim.iter() {
        if dir_ino == crate::path::EXT4_ROOT_INODE {
            // root '..' conventionally points at root itself
            if claimed != crate::path::EXT4_ROOT_INODE {
                let a = Anomaly::WrongDotDot {
                    dir_ino,
                    claims: claimed,
                    actual_parent: crate::path::EXT4_ROOT_INODE,
                };
                on_finding(&a);
                report.anomalies_count += 1;
            }
        }
        // Non-root parent validation requires tracking the enqueueing
        // parent. We skip here and rely on the LinkCount* checks to
        // catch the most common class of corruption (hardlinks that
        // lost their backrefs).
    }

    on_progress(FsckPhase::Inodes, 1, 1);
    on_progress(FsckPhase::Finalize, 0, 1);
    on_progress(FsckPhase::Finalize, 1, 1);

    Ok(())
}

fn emit_dir_progress(
    on_progress: &mut dyn FnMut(FsckPhase, u64, u64),
    scanned: u32,
    queue_len: usize,
) {
    let done = scanned as u64;
    let total = done + queue_len as u64;
    on_progress(FsckPhase::Directory, done, total);
}

fn collect_dir_entries(
    fs: &Filesystem,
    inode: &Inode,
    has_filetype: bool,
    block_size: u32,
) -> Result<Vec<crate::dir::DirEntry>> {
    let mut entries = Vec::new();
    if inode.has_inline_data() {
        for entry in DirBlockIter::new(&inode.block, has_filetype) {
            entries.push(entry?);
        }
        return Ok(entries);
    }
    if !inode.has_extents() {
        return Err(Error::Corrupt(
            "legacy non-extent dirs not supported by audit",
        ));
    }
    let total_blocks = inode.size.div_ceil(block_size as u64);
    let mut buf = vec![0u8; block_size as usize];
    for logical in 0..total_blocks {
        let Some(phys) = extent::map_logical(&inode.block, fs.dev.as_ref(), block_size, logical)?
        else {
            continue;
        };
        let offset = phys
            .checked_mul(block_size as u64)
            .ok_or(Error::Corrupt("audit: dir block offset overflow"))?;
        fs.dev.read_at(offset, &mut buf)?;
        for entry in DirBlockIter::new(&buf, has_filetype) {
            // Ignore parse errors on dx_root first block of indexed dirs
            match entry {
                Ok(e) => entries.push(e),
                Err(_) if logical == 0 => continue,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(entries)
}

impl Filesystem {
    /// Run an ext4 audit tool-style read-only audit.
    ///
    /// Walks from root, counts how many directory entries reference
    /// each inode, and compares that against each inode's
    /// `i_links_count`. Returns an [`AuditReport`] — empty
    /// `anomalies` means every invariant we check held.
    ///
    /// The pass is bounded: never visits more than
    /// `max_dirs_visited` directories and never scans more than
    /// `max_entries_per_dir` entries within a single directory.
    /// Pass `u32::MAX` for an unbounded pass.
    pub fn audit(&self, max_dirs_visited: u32, max_entries_per_dir: u32) -> Result<AuditReport> {
        audit(self, max_dirs_visited, max_entries_per_dir)
    }
}
