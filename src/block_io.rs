//! Abstract block-device I/O.
//!
//! The driver doesn't care if blocks come from a file, raw device, or a
//! callback into Swift — it just needs `read_at(offset, buf) -> Result<()>`.
//!
//! `write_at` is an optional trait method: it defaults to returning
//! `Error::Corrupt("read-only device")` so every existing read-only caller
//! keeps working. `FileDevice` and the callback-with-writer device override
//! it when the underlying resource allows writes.

use crate::error::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

/// Random-access block device. Reads required; writes optional.
pub trait BlockDevice: Send + Sync {
    /// Read exactly `buf.len()` bytes starting at `offset` (bytes from start of device).
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Total device size in bytes (for bounds-checking).
    fn size_bytes(&self) -> u64;

    /// Write exactly `buf.len()` bytes at `offset`. Default: returns an error
    /// for read-only devices. Writable devices override this.
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<()> {
        Err(Error::Corrupt(
            "block device is read-only (no write_at impl)",
        ))
    }

    /// Flush any pending writes to stable storage. Default: no-op for
    /// read-only devices; writable devices should implement fsync semantics.
    fn flush(&self) -> Result<()> {
        Ok(())
    }

    /// Reports whether `write_at` is likely to succeed. Used by the mount
    /// path to decide whether journal replay is possible.
    fn is_writable(&self) -> bool {
        false
    }
}

/// File-backed device — used for disk images and `/dev/diskN`.
pub struct FileDevice {
    file: Mutex<File>,
    size: u64,
    writable: bool,
}

impl FileDevice {
    /// Open read-only. Matches pre-existing behaviour.
    pub fn open(path: &str) -> Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            file: Mutex::new(file),
            size,
            writable: false,
        })
    }

    /// Open read-write. Prefer this when the caller needs to journal-replay
    /// or apply Phase 4 mutations. Falls back to an error if the path is
    /// not writable.
    pub fn open_rw(path: &str) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            file: Mutex::new(file),
            size,
            writable: true,
        })
    }

    /// Open read-write if possible; otherwise fall back to read-only. Useful
    /// for the mount path so read-only images on e.g. a locked volume still
    /// mount, just without replay.
    pub fn open_best_effort(path: &str) -> Result<Self> {
        match Self::open_rw(path) {
            Ok(d) => Ok(d),
            Err(_) => Self::open(path),
        }
    }
}

impl BlockDevice for FileDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(buf)?;
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(Error::Corrupt("FileDevice opened read-only"));
        }
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(buf)?;
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        if !self.writable {
            return Ok(());
        }
        let mut f = self.file.lock().unwrap();
        f.flush()?;
        f.sync_data()?;
        Ok(())
    }

    fn is_writable(&self) -> bool {
        self.writable
    }
}

/// Read callback: fill `buf` starting at byte `offset`.
pub type ReadCb = Box<dyn Fn(u64, &mut [u8]) -> std::io::Result<()> + Send + Sync>;
/// Write callback: write `buf` starting at byte `offset`.
pub type WriteCb = Box<dyn Fn(u64, &[u8]) -> std::io::Result<()> + Send + Sync>;
/// Flush callback.
pub type FlushCb = Box<dyn Fn() -> std::io::Result<()> + Send + Sync>;

/// Callback-backed device — used when the host process owns the fd
/// (e.g. FSBlockDeviceResource via the C bridge). Optional write callback;
/// set to `None` for read-only.
pub struct CallbackDevice {
    pub size: u64,
    pub read: ReadCb,
    pub write: Option<WriteCb>,
    pub flush: Option<FlushCb>,
}

impl BlockDevice for CallbackDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        (self.read)(offset, buf)?;
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<()> {
        match &self.write {
            Some(f) => {
                f(offset, buf)?;
                Ok(())
            }
            None => Err(Error::Corrupt("CallbackDevice has no write callback")),
        }
    }

    fn flush(&self) -> Result<()> {
        match &self.flush {
            Some(f) => {
                f()?;
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn is_writable(&self) -> bool {
        self.write.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_image(bytes: &[u8]) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/ext4rs_block_io_test_{}_{n}.img", std::process::id());
        let mut f = File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn file_device_ro_write_rejected() {
        let path = tmp_image(&[0u8; 4096]);
        let dev = FileDevice::open(&path).unwrap();
        assert!(!dev.is_writable());
        let err = dev.write_at(0, &[1u8; 16]).unwrap_err();
        match err {
            Error::Corrupt(msg) => assert!(msg.contains("read-only")),
            _ => panic!(),
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn file_device_rw_round_trip() {
        let path = tmp_image(&[0u8; 4096]);
        let dev = FileDevice::open_rw(&path).unwrap();
        assert!(dev.is_writable());
        dev.write_at(100, &[0xAB, 0xCD, 0xEF]).unwrap();
        dev.flush().unwrap();
        let mut buf = [0u8; 3];
        dev.read_at(100, &mut buf).unwrap();
        assert_eq!(buf, [0xAB, 0xCD, 0xEF]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn best_effort_falls_back_to_ro() {
        // Create a file without write permission.
        let path = tmp_image(&[0u8; 4096]);
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&path, perm).unwrap();

        let dev = FileDevice::open_best_effort(&path).unwrap();
        assert!(
            !dev.is_writable(),
            "read-only file should not report writable"
        );
        // Cleanup: restore writability so remove_file succeeds.
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&path, perm).unwrap();
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn callback_device_without_writer_rejects_writes() {
        let dev = CallbackDevice {
            size: 4096,
            read: Box::new(|_, buf| {
                buf.fill(0);
                Ok(())
            }),
            write: None,
            flush: None,
        };
        assert!(!dev.is_writable());
        assert!(dev.write_at(0, &[0u8; 4]).is_err());
    }
}
