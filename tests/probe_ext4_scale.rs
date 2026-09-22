//! Scratch probe (not for commit): grow ext4 structures until they change
//! shape, with e2fsck judging each phase.

use fs_ext4::block_io::FileDevice;
use fs_ext4::fs::Filesystem;
use std::process::Command;
use std::sync::Arc;

fn tool(name: &str) -> String {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|p| std::path::Path::new(p).exists())
        .unwrap_or_else(|| panic!("{name} is not installed"))
}

fn e2fsck(image: &str, what: &str) {
    let out = Command::new(tool("e2fsck"))
        .args(["-fn", image])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "[{what}] e2fsck rejected the volume:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn mount(image: &str) -> Filesystem {
    Filesystem::mount(Arc::new(FileDevice::open_rw(image).unwrap())).unwrap()
}

fn run(tag: &str, mkfs_args: &[&str], size_mib: u64) {
    let image = format!(
        "{}/probe-{tag}-{}.img",
        std::env::temp_dir().display(),
        std::process::id()
    );
    std::fs::File::create(&image)
        .and_then(|f| f.set_len(size_mib * 1024 * 1024))
        .unwrap();
    let out = Command::new(tool("mkfs.ext4"))
        .args(["-q", "-F"])
        .args(mkfs_args)
        .arg(&image)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    e2fsck(&image, "as built");

    let mut refusals: Vec<String> = Vec::new();
    let mut note = |what: String, r: Result<(), fs_ext4::Error>, refusals: &mut Vec<String>| {
        if let Err(e) = r {
            refusals.push(format!("{what} -> {e:?}"));
        }
    };

    // A directory grown past short, block and one-level index form.
    {
        let fs = mount(&image);
        note(
            "mkdir /big".into(),
            fs.apply_mkdir("/big", 0o755).map(|_| ()),
            &mut refusals,
        );
        for i in 0..4000 {
            let p = format!("/big/a_name_long_enough_to_fill_leaves_{i:05}");
            note(
                format!("create {p}"),
                fs.apply_create(&p, 0o644).map(|_| ()),
                &mut refusals,
            );
        }
    }
    e2fsck(&image, "after 4000 creates");

    // Data into many of them, so allocation crosses groups.
    {
        let fs = mount(&image);
        for i in (0..4000).step_by(4) {
            let p = format!("/big/a_name_long_enough_to_fill_leaves_{i:05}");
            note(
                format!("pwrite {p}"),
                fs.apply_pwrite(&p, 0, &vec![0xA5; 5000]).map(|_| ()),
                &mut refusals,
            );
        }
    }
    e2fsck(&image, "after 1000 writes");

    // A deep extent tree: a block every 64 KiB, so each extent is its own
    // record and the tree has to grow levels.
    {
        let fs = mount(&image);
        note(
            "create /deep".into(),
            fs.apply_create("/deep", 0o644).map(|_| ()),
            &mut refusals,
        );
        for i in 0..1500u64 {
            note(
                format!("pwrite /deep at {}", i * 65536),
                fs.apply_pwrite("/deep", i * 65536, &[7u8; 512]).map(|_| ()),
                &mut refusals,
            );
        }
    }
    e2fsck(&image, "after a deep extent tree");

    // Punch every other extent out of it, then fill the holes again.
    {
        let fs = mount(&image);
        let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(i, _)| i);
        let deep =
            fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, "/deep").expect("resolve");
        for i in (0..1500u64).step_by(2) {
            note(
                format!("punch /deep at {}", i * 65536),
                fs.apply_fallocate_punch_hole(deep, i * 65536, 4096),
                &mut refusals,
            );
        }
        for i in (0..1500u64).step_by(2) {
            note(
                format!("refill /deep at {}", i * 65536),
                fs.apply_pwrite("/deep", i * 65536, &[9u8; 512]).map(|_| ()),
                &mut refusals,
            );
        }
    }
    e2fsck(&image, "after punching and refilling");

    // Unlink most of the big directory, which shrinks the index.
    {
        let fs = mount(&image);
        for i in 0..3000 {
            let p = format!("/big/a_name_long_enough_to_fill_leaves_{i:05}");
            note(format!("unlink {p}"), fs.apply_unlink(&p), &mut refusals);
        }
    }
    e2fsck(&image, "after 3000 unlinks");

    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for r in &refusals {
        let kind = r.split(" -> ").nth(1).unwrap_or("?").to_string();
        *kinds.entry(kind).or_default() += 1;
    }
    eprintln!("[{tag}] {} refusals: {:#?}", refusals.len(), kinds);
    let _ = std::fs::remove_file(&image);
}

#[test]
fn probe_default_4k() {
    run("default", &["-b", "4096"], 512);
}

#[test]
fn probe_1k_blocks() {
    run("b1024", &["-b", "1024"], 512);
}

#[test]
fn probe_block_mapped() {
    run(
        "noextent",
        &["-b", "4096", "-O", "^extent,^64bit,^metadata_csum"],
        512,
    );
}
