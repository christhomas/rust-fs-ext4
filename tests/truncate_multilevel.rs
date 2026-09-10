//! Real ext4 trees must support the same shrink/unlink lifecycle as inline trees.
use fs_ext4::{block_io::FileDevice, extent::ExtentHeader, Filesystem};
use std::{path::PathBuf, sync::Arc};

struct Memory(std::sync::Mutex<Vec<u8>>);
impl fs_ext4::block_io::BlockDevice for Memory {
    fn size_bytes(&self) -> u64 {
        self.0.lock().unwrap().len() as u64
    }
    fn is_writable(&self) -> bool {
        true
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> fs_ext4::Result<()> {
        out.copy_from_slice(&self.0.lock().unwrap()[offset as usize..offset as usize + out.len()]);
        Ok(())
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> fs_ext4::Result<()> {
        self.0.lock().unwrap()[offset as usize..offset as usize + bytes.len()]
            .copy_from_slice(bytes);
        Ok(())
    }
}

fn scratch(label: &str) -> Option<PathBuf> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-disks/ext4-deep-extents.img");
    if !source.exists() {
        eprintln!("skip: build test-disks/ext4-deep-extents.img to run this oracle");
        return None;
    }
    let path = std::env::temp_dir().join(format!("ext4-deep-{label}-{}.img", std::process::id()));
    std::fs::copy(source, &path).unwrap();
    Some(path)
}
fn inode(fs: &Filesystem, name: &str) -> u32 {
    let mut read = |ino| fs.read_inode_verified(ino).map(|(inode, _)| inode);
    fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut read, name).unwrap()
}
fn oracle(path: &std::path::Path) {
    if let Ok(output) = std::process::Command::new("e2fsck")
        .args(["-fn"])
        .arg(path)
        .output()
    {
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
#[test]
fn equal_size_deep_truncate_leaves_image_unchanged() {
    let Some(path) = scratch("equal") else {
        return;
    };
    let fs = Filesystem::mount(Arc::new(
        FileDevice::open_rw(path.to_str().unwrap()).unwrap(),
    ))
    .unwrap();
    let ino = inode(&fs, "/sparse.bin");
    let (before, _) = fs.read_inode_verified(ino).unwrap();
    assert!(ExtentHeader::parse(&before.block).unwrap().depth > 0);
    let bytes = std::fs::read(&path).unwrap();
    fs.apply_truncate_shrink(ino, before.size).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    drop(fs);
    oracle(&path);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn deep_shrink_and_unlink_free_tree_blocks() {
    let Some(path) = scratch("shrink") else {
        return;
    };
    let fs = Filesystem::mount(Arc::new(
        FileDevice::open_rw(path.to_str().unwrap()).unwrap(),
    ))
    .unwrap();
    let ino = inode(&fs, "/sparse.bin");
    fs.apply_truncate_shrink(ino, 65_537).unwrap();
    let (after, _) = fs.read_inode_verified(ino).unwrap();
    assert_eq!(after.size, 65_537);
    drop(fs);
    oracle(&path);
    let fs = Filesystem::mount(Arc::new(
        FileDevice::open_rw(path.to_str().unwrap()).unwrap(),
    ))
    .unwrap();
    fs.apply_unlink("/sparse.bin").unwrap();
    drop(fs);
    oracle(&path);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn depth_two_shrink_preserves_bytes_and_reclaims_every_tree_node() {
    let Some(path) = scratch("depth2") else {
        return;
    };
    let device = Arc::new(Memory(std::sync::Mutex::new(std::fs::read(&path).unwrap())));
    let fs = Filesystem::mount(device.clone()).unwrap();
    let ino = fs.apply_create("/fragmented", 0o600).unwrap();
    for n in 0..1500u64 {
        fs.apply_pwrite("/fragmented", n * 8192, &vec![(n % 251) as u8; 4096])
            .unwrap();
    }
    let (original, _) = fs.read_inode_verified(ino).unwrap();
    assert!(ExtentHeader::parse(&original.block).unwrap().depth >= 2);
    let target = 900 * 8192 + 19;
    fs.apply_truncate_shrink(ino, target).unwrap();
    let (after, _) = fs.read_inode_verified(ino).unwrap();
    let mut last = [0; 19];
    fs_ext4::file_io::read(&fs, &after, target - 19, 19, &mut last).unwrap();
    assert_eq!(last, [(900 % 251) as u8; 19]);
    fs.apply_truncate_grow(ino, target + 1024).unwrap();
    let (grown, _) = fs.read_inode_verified(ino).unwrap();
    let mut tail = [1; 1024];
    fs_ext4::file_io::read(&fs, &grown, target, 1024, &mut tail).unwrap();
    assert_eq!(tail, [0; 1024]);
    std::fs::write(&path, &*device.0.lock().unwrap()).unwrap();
    oracle(&path);
    fs.apply_unlink("/fragmented").unwrap();
    std::fs::write(&path, &*device.0.lock().unwrap()).unwrap();
    oracle(&path);
    drop(fs);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn densely_written_fragmented_file_finishes_and_can_be_removed() {
    let Some(path) = scratch("dense-fragmented") else {
        return;
    };
    let memory = Arc::new(Memory(std::sync::Mutex::new(std::fs::read(&path).unwrap())));
    let fs = Filesystem::mount(memory.clone()).unwrap();
    let ino = fs.apply_create("/efisp.fat", 0o600).unwrap();
    fs.apply_create("/unrelated", 0o600).unwrap();
    for n in 0..80u64 {
        fs.apply_pwrite("/efisp.fat", n * 4096, &vec![(n % 251) as u8; 4096])
            .unwrap();
        fs.apply_pwrite("/unrelated", n * 4096, &[0x5a; 4096])
            .unwrap();
    }
    let (file, _) = fs.read_inode_verified(ino).unwrap();
    assert!(ExtentHeader::parse(&file.block).unwrap().depth > 0);
    let before = memory.0.lock().unwrap().clone();
    fs.apply_truncate_shrink(ino, file.size).unwrap();
    assert_eq!(*memory.0.lock().unwrap(), before);
    fs.apply_unlink("/efisp.fat").unwrap();
    let (unrelated, _) = fs.read_inode_verified(inode(&fs, "/unrelated")).unwrap();
    assert_eq!(
        fs_ext4::file_io::read_all(&fs, &unrelated).unwrap(),
        vec![0x5a; 80 * 4096]
    );
    std::fs::write(&path, &*memory.0.lock().unwrap()).unwrap();
    oracle(&path);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn invalid_deep_checksum_refuses_shrink_and_unlink_without_writing() {
    let Some(path) = scratch("corrupt") else {
        return;
    };
    let device = Arc::new(Memory(std::sync::Mutex::new(std::fs::read(&path).unwrap())));
    let fs = Filesystem::mount(device.clone()).unwrap();
    let ino = inode(&fs, "/sparse.bin");
    let (entry, _) = fs.read_inode_verified(ino).unwrap();
    let child = fs_ext4::extent::ExtentIdx::parse(&entry.block[12..24])
        .unwrap()
        .leaf_block;
    drop(fs);
    device.0.lock().unwrap()[child as usize * 4096 + 4095] ^= 1;
    let fs = Filesystem::mount(device.clone()).unwrap();
    let before = device.0.lock().unwrap().clone();
    assert!(matches!(
        fs.apply_truncate_shrink(ino, 0),
        Err(fs_ext4::Error::BadChecksum { .. })
    ));
    assert_eq!(*device.0.lock().unwrap(), before);
    assert!(matches!(
        fs.apply_unlink("/sparse.bin"),
        Err(fs_ext4::Error::BadChecksum { .. })
    ));
    assert_eq!(*device.0.lock().unwrap(), before);
    std::fs::remove_file(path).unwrap();
}

struct Interrupted {
    memory: Arc<Memory>,
    writes: std::sync::atomic::AtomicUsize,
    budget: std::sync::atomic::AtomicUsize,
}
impl fs_ext4::block_io::BlockDevice for Interrupted {
    fn size_bytes(&self) -> u64 {
        self.memory.size_bytes()
    }
    fn is_writable(&self) -> bool {
        true
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> fs_ext4::Result<()> {
        self.memory.read_at(offset, out)
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> fs_ext4::Result<()> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.writes.fetch_add(1, Relaxed) >= self.budget.load(Relaxed) {
            return Err(std::io::Error::other("injected interruption").into());
        }
        self.memory.write_at(offset, bytes)
    }
}

#[test]
fn deep_shrink_interrupted_at_each_write_recovers_original_or_complete() {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    let Some(path) = scratch("interrupt") else {
        return;
    };
    let baseline = std::fs::read(&path).unwrap();
    let mut writes = usize::MAX;
    // The first successful pass discovers the real transaction write count.
    // Then cut every write boundary and check the independent ext4 oracle.
    for budget in std::iter::once(usize::MAX).chain(0..64) {
        if budget != usize::MAX && budget > writes {
            break;
        }
        let memory = Arc::new(Memory(std::sync::Mutex::new(baseline.clone())));
        let fault = Arc::new(Interrupted {
            memory: memory.clone(),
            writes: AtomicUsize::new(0),
            budget: AtomicUsize::new(usize::MAX),
        });
        let fs = Filesystem::mount(fault.clone()).unwrap();
        let ino = inode(&fs, "/sparse.bin");
        let (old, _) = fs.read_inode_verified(ino).unwrap();
        fault.writes.store(0, Relaxed);
        fault.budget.store(budget, Relaxed);
        let result = fs.apply_truncate_shrink(ino, 65_537);
        if budget == usize::MAX {
            result.unwrap();
            writes = fault.writes.load(Relaxed);
            assert!(writes < 64);
        } else if budget < writes {
            assert!(result.is_err());
        }
        drop(fs);
        let recovered = Filesystem::mount(memory.clone()).unwrap();
        let (after, _) = recovered.read_inode_verified(ino).unwrap();
        assert!(
            after.size == old.size || after.size == 65_537,
            "torn inode at write {budget}"
        );
        drop(recovered);
        std::fs::write(&path, &*memory.0.lock().unwrap()).unwrap();
        oracle(&path);
    }
    std::fs::remove_file(path).unwrap();
}
