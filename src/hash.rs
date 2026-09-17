//! HTree hash functions for indexed directory lookup.
//!
//! Spec: kernel.org/doc/html/latest/filesystems/ext4/directory.html#hash-tree-directories
//!
//! When an ext4 directory has the `EXT4_INDEX_FL` flag, lookups go through a
//! hash tree. The hash version (in the `dx_root_info` of the directory's
//! first block) selects the algorithm used to compute a 32-bit hash from a
//! filename. The hash drives a b+tree descent to the leaf block containing
//! the entry (or NOT — leaves are still scanned linearly).
//!
//! Hash versions:
//!   0 = legacy (signed)
//!   1 = half_md4 (signed)
//!   2 = tea (signed)
//!   3 = legacy (unsigned)        — set when SUPERBLOCK flag UNSIGNED_HASH on
//!   4 = half_md4 (unsigned)
//!   5 = tea (unsigned)
//!
//! All hashes use the four 32-bit `s_hash_seed` words from the superblock as
//! initial state (or a constant default if the seed is all-zero).
//!
//! After computing, the low bit is cleared so it can never collide with the
//! HTREE_EOF sentinel value `0xFFFFFFFE`.
//!
//! The Linux implementation hashes 4-byte words built from the name; the
//! `unsigned` variants treat each byte as `u8`, the signed variants treat
//! each byte as `i8` (sign-extended into i32 then back to u32 — same bit
//! pattern as `as i8 as i32 as u32`). This matters for non-ASCII names.

#![allow(clippy::many_single_char_names)]

/// Sentinel returned when the htree walk reaches the last block.
pub const HTREE_EOF: u32 = 0xFFFF_FFFE;

/// Hash version codes (low byte of `dx_root_info.hash_version`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashVersion {
    Legacy = 0,
    HalfMd4 = 1,
    Tea = 2,
    LegacyUnsigned = 3,
    HalfMd4Unsigned = 4,
    TeaUnsigned = 5,
}

impl HashVersion {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Legacy),
            1 => Some(Self::HalfMd4),
            2 => Some(Self::Tea),
            3 => Some(Self::LegacyUnsigned),
            4 => Some(Self::HalfMd4Unsigned),
            5 => Some(Self::TeaUnsigned),
            _ => None,
        }
    }

    pub fn is_unsigned(self) -> bool {
        matches!(
            self,
            Self::LegacyUnsigned | Self::HalfMd4Unsigned | Self::TeaUnsigned
        )
    }
}

/// Result of `name_hash` — the major hash drives tree descent, the minor
/// hash differentiates entries that share the same major (collision tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameHash {
    pub major: u32,
    pub minor: u32,
}

/// Compute the htree hash of `name` using `version` and the superblock seed.
///
/// A transcription of the kernel's `ext4fs_dirhash` (`fs/ext4/hash.c`),
/// checked value for value against e2fsprogs' `dx_hash` in
/// `tests/htree_hash_vectors.rs`.
pub fn name_hash(name: &[u8], version: HashVersion, seed: &[u32; 4]) -> NameHash {
    let signed = !version.is_unsigned();
    let (major, minor) = match version {
        HashVersion::Legacy | HashVersion::LegacyUnsigned => (legacy_hash(name, signed), 0),
        HashVersion::HalfMd4 | HashVersion::HalfMd4Unsigned => half_md4(name, signed, seed),
        HashVersion::Tea | HashVersion::TeaUnsigned => tea_hash(name, signed, seed),
    };

    let major = clear_eof(major);
    NameHash { major, minor }
}

/// Clear the low bit, and move the one value that would still equal the
/// end-of-directory marker (`EXT4_HTREE_EOF_32BIT << 1`) down a step.
fn clear_eof(major: u32) -> u32 {
    let major = major & !1;
    if major == HTREE_EOF {
        HTREE_EOF - 2
    } else {
        major
    }
}

/// The hash version a directory actually uses.
///
/// `dx_root_info.hash_version` only ever records the signed codes 0-2.
/// Whether the filesystem hashed names as signed or unsigned bytes is the
/// superblock's `s_flags` (`EXT2_FLAGS_UNSIGNED_HASH`, 0x2), fixed by
/// `mke2fs` from the platform's `char`: signed on x86, unsigned on ARM. The
/// kernel adds 3 to a signed code when that flag is set, and so does this.
pub fn effective_version(root_version: HashVersion, unsigned_hash: bool) -> HashVersion {
    match (root_version, unsigned_hash) {
        (HashVersion::Legacy, true) => HashVersion::LegacyUnsigned,
        (HashVersion::HalfMd4, true) => HashVersion::HalfMd4Unsigned,
        (HashVersion::Tea, true) => HashVersion::TeaUnsigned,
        (v, _) => v,
    }
}

// ---------------------------------------------------------------------------
// Legacy hash
// ---------------------------------------------------------------------------

/// `dx_hack_hash` — the original ext3 hash. Single 32-bit output.
fn legacy_hash(name: &[u8], signed: bool) -> u32 {
    let mut hash0: u32 = 0x12A3_FE2D;
    let mut hash1: u32 = 0x37AB_E8F9;
    for &b in name {
        let c = if signed { (b as i8) as i32 } else { b as i32 };
        let mut hash = hash1.wrapping_add(hash0 ^ (c.wrapping_mul(7_152_373) as u32));
        if hash & 0x8000_0000 != 0 {
            hash = hash.wrapping_sub(0x7FFF_FFFF);
        }
        hash1 = hash0;
        hash0 = hash;
    }
    hash0 << 1
}

// ---------------------------------------------------------------------------
// Helpers shared by half_md4 + tea
// ---------------------------------------------------------------------------

/// `str2hashbuf`: pack up to `buf.len() * 4` bytes of `msg` into words.
///
/// Each byte is shifted in from the bottom (`val = c + (val << 8)`), so the
/// FIRST byte of a word ends up in its HIGH eight bits. Every word starts from
/// the pad, built from the length of the name remaining from `msg` onwards,
/// and a final partial word keeps the pad's bytes above the name's. Words
/// after the name are the pad itself. (Only the last chunk of a name can be
/// partial, so the remaining length and the chunk's length never give
/// different hashes.)
fn str2hashbuf(msg: &[u8], buf: &mut [u32], signed: bool) {
    let len = msg.len() as u32;
    let mut pad = len | (len << 8);
    pad |= pad << 16;
    let mut val = pad;
    let take = msg.len().min(buf.len() * 4);
    let mut slot = 0;
    for (i, &b) in msg[..take].iter().enumerate() {
        let c = if signed {
            (b as i8) as i32 as u32
        } else {
            b as u32
        };
        val = c.wrapping_add(val << 8);
        if i % 4 == 3 {
            buf[slot] = val;
            slot += 1;
            val = pad;
        }
    }
    if slot < buf.len() {
        buf[slot] = val;
        slot += 1;
    }
    for word in &mut buf[slot..] {
        *word = pad;
    }
}

// ---------------------------------------------------------------------------
// TEA hash
// ---------------------------------------------------------------------------

/// One round of TEA on a 4-byte message block (transformed into 2x u32) with
/// 4 key words. Updates `hash[0]` and `hash[1]` (the "buf" in dx_hash.c).
fn tea_transform(hash: &mut [u32; 4], data: &[u32; 4]) {
    let mut sum: u32 = 0;
    let delta: u32 = 0x9E3779B9;
    let mut h0 = hash[0];
    let mut h1 = hash[1];
    for _ in 0..16 {
        sum = sum.wrapping_add(delta);
        let t1 = ((h1 << 4).wrapping_add(data[0]))
            ^ (h1.wrapping_add(sum))
            ^ ((h1 >> 5).wrapping_add(data[1]));
        h0 = h0.wrapping_add(t1);
        let t2 = ((h0 << 4).wrapping_add(data[2]))
            ^ (h0.wrapping_add(sum))
            ^ ((h0 >> 5).wrapping_add(data[3]));
        h1 = h1.wrapping_add(t2);
    }
    hash[0] = hash[0].wrapping_add(h0);
    hash[1] = hash[1].wrapping_add(h1);
}

fn tea_hash(name: &[u8], signed: bool, seed: &[u32; 4]) -> (u32, u32) {
    let mut hash = init_state(seed);
    let mut block = [0u32; 4];
    // The kernel's `while (len > 0)`: an empty name runs no rounds.
    for start in (0..name.len()).step_by(16) {
        str2hashbuf(&name[start..], &mut block, signed);
        tea_transform(&mut hash, &block);
    }
    (hash[0], hash[1])
}

// ---------------------------------------------------------------------------
// half_md4 (a stripped-down MD4 variant)
// ---------------------------------------------------------------------------

#[inline]
fn rol(x: u32, n: u32) -> u32 {
    x.rotate_left(n)
}

#[inline]
fn f(x: u32, y: u32, z: u32) -> u32 {
    z ^ (x & (y ^ z))
}
#[inline]
fn g(x: u32, y: u32, z: u32) -> u32 {
    (x & y).wrapping_add((x ^ y) & z)
}
#[inline]
fn h(x: u32, y: u32, z: u32) -> u32 {
    x ^ y ^ z
}

fn ff(a: &mut u32, b: u32, c: u32, d: u32, x: u32, s: u32) {
    *a = a.wrapping_add(f(b, c, d)).wrapping_add(x);
    *a = rol(*a, s);
}
fn gg(a: &mut u32, b: u32, c: u32, d: u32, x: u32, s: u32) {
    *a = a
        .wrapping_add(g(b, c, d))
        .wrapping_add(x)
        .wrapping_add(0x5A82_7999);
    *a = rol(*a, s);
}
fn hh(a: &mut u32, b: u32, c: u32, d: u32, x: u32, s: u32) {
    *a = a
        .wrapping_add(h(b, c, d))
        .wrapping_add(x)
        .wrapping_add(0x6ED9_EBA1);
    *a = rol(*a, s);
}

/// half_md4_transform — Linux's stripped MD4 variant. Updates `hash` and
/// returns nothing; the data is consumed in two halves (8 words = 32 bytes).
fn half_md4_transform(hash: &mut [u32; 4], data: [u32; 8]) -> u32 {
    let (mut a, mut b, mut c, mut d) = (hash[0], hash[1], hash[2], hash[3]);

    // Round 1
    ff(&mut a, b, c, d, data[0], 3);
    ff(&mut d, a, b, c, data[1], 7);
    ff(&mut c, d, a, b, data[2], 11);
    ff(&mut b, c, d, a, data[3], 19);
    ff(&mut a, b, c, d, data[4], 3);
    ff(&mut d, a, b, c, data[5], 7);
    ff(&mut c, d, a, b, data[6], 11);
    ff(&mut b, c, d, a, data[7], 19);

    // Round 2
    gg(&mut a, b, c, d, data[1], 3);
    gg(&mut d, a, b, c, data[3], 5);
    gg(&mut c, d, a, b, data[5], 9);
    gg(&mut b, c, d, a, data[7], 13);
    gg(&mut a, b, c, d, data[0], 3);
    gg(&mut d, a, b, c, data[2], 5);
    gg(&mut c, d, a, b, data[4], 9);
    gg(&mut b, c, d, a, data[6], 13);

    // Round 3
    hh(&mut a, b, c, d, data[3], 3);
    hh(&mut d, a, b, c, data[7], 9);
    hh(&mut c, d, a, b, data[2], 11);
    hh(&mut b, c, d, a, data[6], 15);
    hh(&mut a, b, c, d, data[1], 3);
    hh(&mut d, a, b, c, data[5], 9);
    hh(&mut c, d, a, b, data[0], 11);
    hh(&mut b, c, d, a, data[4], 15);

    hash[0] = hash[0].wrapping_add(a);
    hash[1] = hash[1].wrapping_add(b);
    hash[2] = hash[2].wrapping_add(c);
    hash[3] = hash[3].wrapping_add(d);
    hash[1] // major hash — kernel returns buf[1]
}

fn half_md4(name: &[u8], signed: bool, seed: &[u32; 4]) -> (u32, u32) {
    let mut hash = init_state(seed);
    let mut block = [0u32; 8];
    for start in (0..name.len()).step_by(32) {
        str2hashbuf(&name[start..], &mut block, signed);
        half_md4_transform(&mut hash, block);
    }
    (hash[1], hash[2])
}

// ---------------------------------------------------------------------------
// Common init
// ---------------------------------------------------------------------------

/// Initial hash state. If the superblock seed is all-zero, use the spec
/// default constants; otherwise use the seed verbatim.
fn init_state(seed: &[u32; 4]) -> [u32; 4] {
    if seed.iter().all(|&w| w == 0) {
        [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476]
    } else {
        *seed
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_seed() -> [u32; 4] {
        [0; 4]
    }

    #[test]
    fn legacy_distinguishes_names() {
        let s = empty_seed();
        let h1 = name_hash(b"foo", HashVersion::Legacy, &s);
        let h2 = name_hash(b"bar", HashVersion::Legacy, &s);
        let h3 = name_hash(b"foo", HashVersion::Legacy, &s);
        assert_ne!(h1.major, h2.major, "different names → different majors");
        assert_eq!(h1, h3, "deterministic");
    }

    #[test]
    fn tea_distinguishes_names() {
        let s = empty_seed();
        let h1 = name_hash(b"hello", HashVersion::Tea, &s);
        let h2 = name_hash(b"world", HashVersion::Tea, &s);
        assert_ne!(h1.major, h2.major);
        assert_ne!(h1.minor, h2.minor);
    }

    #[test]
    fn half_md4_distinguishes_names() {
        let s = empty_seed();
        let h1 = name_hash(b"document.txt", HashVersion::HalfMd4, &s);
        let h2 = name_hash(b"document.bak", HashVersion::HalfMd4, &s);
        assert_ne!(h1.major, h2.major);
    }

    #[test]
    fn low_bit_always_clear() {
        let s = empty_seed();
        for name in [b"a".as_slice(), b"ab", b"abc", b"abcdefghij", b""] {
            for v in [HashVersion::Legacy, HashVersion::HalfMd4, HashVersion::Tea] {
                let h = name_hash(name, v, &s);
                assert_eq!(h.major & 1, 0, "version={v:?} name={name:?}");
            }
        }
    }

    #[test]
    fn signed_vs_unsigned_diverge_on_high_bytes() {
        let s = empty_seed();
        let name = &[0xC3, 0xA9]; // "é" in UTF-8 → high-bit bytes
        let signed = name_hash(name, HashVersion::Legacy, &s);
        let unsigned = name_hash(name, HashVersion::LegacyUnsigned, &s);
        assert_ne!(signed.major, unsigned.major);
    }

    #[test]
    fn version_recognition_round_trip() {
        for v in 0..=5 {
            let parsed = HashVersion::from_u8(v).expect("known version");
            assert_eq!(parsed as u8, v);
        }
        assert!(HashVersion::from_u8(99).is_none());
    }

    #[test]
    fn empty_name_does_not_panic() {
        let s = empty_seed();
        let _ = name_hash(b"", HashVersion::Legacy, &s);
        let _ = name_hash(b"", HashVersion::Tea, &s);
        let _ = name_hash(b"", HashVersion::HalfMd4, &s);
    }

    // --- init_state ---

    #[test]
    fn init_state_zero_seed_returns_md4_constants() {
        let state = init_state(&[0; 4]);
        assert_eq!(state, [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476]);
    }

    #[test]
    fn init_state_nonzero_seed_returns_verbatim() {
        let seed = [0xDEAD_BEEF, 0xCAFE_BABE, 0x1234_5678, 0x9ABC_DEF0];
        assert_eq!(init_state(&seed), seed);
    }

    #[test]
    fn init_state_partial_nonzero_is_not_zero_seed() {
        // Only one word non-zero — still uses seed verbatim, not defaults.
        let seed = [0, 0, 0, 1];
        assert_ne!(init_state(&seed)[0], 0x6745_2301);
        assert_eq!(init_state(&seed), seed);
    }

    // --- rol ---

    #[test]
    fn rol_shift_by_one() {
        assert_eq!(rol(1, 1), 2);
        assert_eq!(rol(0x8000_0000, 1), 1); // wraps MSB to LSB
    }

    #[test]
    fn rol_full_rotation_is_identity() {
        assert_eq!(rol(0xDEAD_BEEF, 32), 0xDEAD_BEEF);
    }

    #[test]
    fn rol_matches_rotate_left() {
        assert_eq!(rol(0x1234_5678, 7), 0x1234_5678u32.rotate_left(7));
    }

    // --- f / g / h ---

    #[test]
    fn f_acts_as_mux() {
        // f(x, y, z) = z ^ (x & (y ^ z))
        // When x=all-ones: f = z ^ (y^z) = y
        assert_eq!(f(u32::MAX, 0xAAAA_AAAA, 0x5555_5555), 0xAAAA_AAAA);
        // When x=all-zeros: f = z ^ 0 = z
        assert_eq!(f(0, 0xAAAA_AAAA, 0x5555_5555), 0x5555_5555);
    }

    #[test]
    fn g_is_majority() {
        // g(x, y, z) = (x&y) + ((x^y)&z) — majority function
        // majority of (1,1,0): expect 1 (1&1 + (1^1)&0 = 1 + 0 = 1)
        assert_eq!(g(1, 1, 0), 1);
        // majority of (0,0,1): expect 0
        assert_eq!(g(0, 0, 1), 0);
        // majority of (1,1,1): all-ones
        assert_eq!(g(u32::MAX, u32::MAX, u32::MAX), u32::MAX);
    }

    #[test]
    fn h_is_xor3() {
        assert_eq!(
            h(0xAAAA_AAAA, 0x5555_5555, 0xF0F0_F0F0),
            0xAAAA_AAAA ^ 0x5555_5555 ^ 0xF0F0_F0F0
        );
        // x ^ x ^ x = (x^x) ^ x = 0 ^ x = x
        let x = 0xDEAD_BEEF_u32;
        assert_eq!(h(x, x, x), x);
        assert_eq!(h(0, 0, 0), 0);
        assert_eq!(h(u32::MAX, 0, 0), u32::MAX);
    }

    // --- legacy_hash ---

    #[test]
    fn legacy_hash_empty_returns_initial() {
        // No iterations: the initial accumulator, shifted left one as the
        // kernel's `dx_hack_hash` returns it.
        assert_eq!(legacy_hash(b"", true), 0x12A3_FE2D << 1);
        assert_eq!(legacy_hash(b"", false), 0x12A3_FE2D << 1);
    }

    #[test]
    fn legacy_hash_deterministic() {
        assert_eq!(legacy_hash(b"hello", true), legacy_hash(b"hello", true));
    }

    #[test]
    fn legacy_hash_differs_by_input() {
        assert_ne!(legacy_hash(b"foo", true), legacy_hash(b"bar", true));
    }

    #[test]
    fn legacy_hash_signed_vs_unsigned_differs_on_high_byte() {
        // 0x80 is -128 as i8, +128 as u8 — signed and unsigned paths diverge.
        let name = &[0x80u8];
        assert_ne!(legacy_hash(name, true), legacy_hash(name, false));
    }

    // --- str2hashbuf ---

    #[test]
    fn str2hashbuf_packs_the_first_byte_highest() {
        let mut buf = [0u32; 1];
        str2hashbuf(b"abcd", &mut buf, false);
        // `val = c + (val << 8)`: 'a' is shifted up past 'b', 'c' and 'd'.
        assert_eq!(buf[0], 0x6162_6364);
    }

    #[test]
    fn str2hashbuf_keeps_the_pad_above_a_partial_word() {
        let mut buf = [0u32; 2];
        // "ab": the pad is the length, 2, in every byte. The partial word
        // starts from it, so the pad's low two bytes survive above "ab".
        str2hashbuf(b"ab", &mut buf, false);
        assert_eq!(buf, [0x0202_6162, 0x0202_0202]);
    }

    #[test]
    fn str2hashbuf_a_partial_word_carries_the_names_length() {
        let mut buf = [0u32; 2];
        str2hashbuf(b"abcdef", &mut buf, false);
        assert_eq!(buf, [0x6162_6364, 0x0606_6566]);
    }

    #[test]
    fn the_end_of_directory_value_is_never_a_hash() {
        assert_eq!(clear_eof(0xFFFF_FFFF), 0xFFFF_FFFC);
        assert_eq!(clear_eof(0xFFFF_FFFE), 0xFFFF_FFFC);
        assert_eq!(clear_eof(0xFFFF_FFFD), 0xFFFF_FFFC);
        assert_eq!(clear_eof(0x1234_5679), 0x1234_5678);
    }

    #[test]
    fn str2hashbuf_empty_is_all_pad() {
        let mut buf = [0u32; 1];
        str2hashbuf(b"", &mut buf, false);
        assert_eq!(buf[0], 0x0000_0000);
    }

    #[test]
    fn a_signed_code_becomes_unsigned_only_under_the_superblock_flag() {
        assert_eq!(
            effective_version(HashVersion::HalfMd4, false),
            HashVersion::HalfMd4
        );
        assert_eq!(
            effective_version(HashVersion::HalfMd4, true),
            HashVersion::HalfMd4Unsigned
        );
        assert_eq!(
            effective_version(HashVersion::Legacy, true),
            HashVersion::LegacyUnsigned
        );
        assert_eq!(
            effective_version(HashVersion::Tea, true),
            HashVersion::TeaUnsigned
        );
        assert_eq!(
            effective_version(HashVersion::TeaUnsigned, false),
            HashVersion::TeaUnsigned
        );
    }

    // --- tea_transform ---

    #[test]
    fn tea_transform_is_deterministic() {
        let mut h1 = [0x6745_2301u32, 0xEFCD_AB89, 0, 0];
        let mut h2 = [0x6745_2301u32, 0xEFCD_AB89, 0, 0];
        let data = [1u32, 2, 3, 4];
        tea_transform(&mut h1, &data);
        tea_transform(&mut h2, &data);
        assert_eq!(h1, h2);
    }

    #[test]
    fn tea_transform_modifies_h0_and_h1() {
        let original = [0x6745_2301u32, 0xEFCD_AB89, 0x1234_5678, 0xABCD_EF01];
        let mut hash = original;
        tea_transform(&mut hash, &[0u32; 4]);
        assert_ne!(hash[0], original[0]);
        assert_ne!(hash[1], original[1]);
        // h[2] and h[3] are key material; tea_transform does not write them
        assert_eq!(hash[2], original[2]);
        assert_eq!(hash[3], original[3]);
    }

    // --- half_md4_transform ---

    #[test]
    fn half_md4_transform_is_deterministic() {
        let mut h1 = [0x6745_2301u32, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];
        let mut h2 = [0x6745_2301u32, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];
        let data = [1u32, 2, 3, 4, 5, 6, 7, 8];
        let r1 = half_md4_transform(&mut h1, data);
        let r2 = half_md4_transform(&mut h2, data);
        assert_eq!(r1, r2);
        assert_eq!(h1, h2);
    }

    #[test]
    fn half_md4_transform_return_equals_hash1() {
        let mut hash = [0x6745_2301u32, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];
        let ret = half_md4_transform(&mut hash, [0u32; 8]);
        assert_eq!(ret, hash[1]);
    }

    #[test]
    fn half_md4_transform_updates_all_words() {
        let original = [0x6745_2301u32, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];
        let mut hash = original;
        half_md4_transform(&mut hash, [1u32; 8]);
        for i in 0..4 {
            assert_ne!(hash[i], original[i], "word {i} should change");
        }
    }
}
