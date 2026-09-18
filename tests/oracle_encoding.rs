//! How an oracle call is written down for the guest's shell.
//!
//! Every tool invocation crosses as text: the tool, its arguments and any
//! standard input, quoted and encoded by the test support crate and read
//! by a shell in the VM. A mistake here is not a compile error and not a
//! test failure somewhere obvious — it is an argument silently split in
//! two, or a `debugfs` script arriving with a line missing. These are the
//! pieces that do it, checked without a VM, so they are in the unit tier.

use fs_ext4_test_support::{guest_base64, guest_quote};

#[test]
fn an_argument_reaches_the_guest_as_one_word() {
    assert_eq!(guest_quote("plain"), "'plain'");
    assert_eq!(guest_quote("two words"), "'two words'");
    assert_eq!(guest_quote("a && b | c"), "'a && b | c'");
    assert_eq!(guest_quote("$HOME"), "'$HOME'");
    assert_eq!(guest_quote("back\\slash"), "'back\\slash'");
    // The one character single quotes cannot hold: close, escape, reopen.
    assert_eq!(guest_quote("it's"), r"'it'\''s'");
    assert_eq!(guest_quote("'"), r"''\'''");
}

/// What the shell then makes of it, which is the claim that matters.
#[test]
fn the_guests_shell_reads_back_exactly_what_was_quoted() {
    for original in [
        "plain",
        "two words",
        "a && b | c",
        "$HOME",
        "it's",
        "back\\slash",
        "/a path/with spaces.img",
        "dump /f /a path/out.bin",
    ] {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", guest_quote(original)))
            .output()
            .expect("run sh");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            original,
            "a shell did not read {original:?} back as one word"
        );
    }
}

#[test]
fn base64_is_the_encoding_base64_d_decodes() {
    // RFC 4648's own vectors, plus the padding cases and a byte no UTF-8
    // string could carry.
    assert_eq!(guest_base64(b""), "");
    assert_eq!(guest_base64(b"f"), "Zg==");
    assert_eq!(guest_base64(b"fo"), "Zm8=");
    assert_eq!(guest_base64(b"foo"), "Zm9v");
    assert_eq!(guest_base64(b"foob"), "Zm9vYg==");
    assert_eq!(guest_base64(b"fooba"), "Zm9vYmE=");
    assert_eq!(guest_base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(guest_base64(b"jo -c\njc\n"), "am8gLWMKamMK");
    assert_eq!(guest_base64(&[0xff, 0x00, 0xfe]), "/wD+");
}

/// And the decoder on the other side agrees, for every byte value.
#[test]
fn every_byte_survives_the_round_trip_through_a_real_decoder() {
    let all: Vec<u8> = (0..=255u8).collect();
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "printf %s {} | base64 -d | od -An -tu1",
            guest_quote(&guest_base64(&all))
        ))
        .output()
        .expect("run base64 -d");
    let decoded: Vec<u8> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(|n| n.parse().expect("a byte"))
        .collect();
    assert_eq!(decoded, all, "base64 -d did not return what was encoded");
}
