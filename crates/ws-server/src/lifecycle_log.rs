//! A small log file of the feed's lifecycle that outlives the container.
//!
//! The 2026-10-15 regular session lost 70 minutes of market data across
//! seven reconnects, and the cause could not be read afterwards: the ws
//! container had been recreated and its json-file log went with it. Docker's
//! own drivers either die with the container (json-file, local) or share the
//! host's journal, which at this service's 1-2 GB a day would push out the
//! host's own logs on a nearly full disk. So the lines that answer "when did
//! the feed stop and why" are also written here, to a file on the persistent
//! data mount, and nothing else is: warnings and errors from everywhere, the
//! `feed` up/down transitions (`market_data::client_feed::mark_feed`), and the
//! upstream connect/subscribe lines (`market_data::ws`).
//!
//! Off unless `WS_LIFECYCLE_LOG` names the file. Bounded: at `MAX_BYTES` the
//! file is renamed to `<name>.1` (replacing the previous one) and restarted,
//! so the pair never exceeds twice that.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub const ENV: &str = "WS_LIFECYCLE_LOG";
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// What goes to the file; the console layer keeps logging everything at info.
pub const FILTER: &str = "warn,feed=info,market_data::ws=info";

pub struct RotatingFile {
    path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
}

impl RotatingFile {
    pub fn open(path: PathBuf, max_bytes: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self { path, file, written, max_bytes })
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        std::fs::rename(&self.path, old)?;
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written >= self.max_bytes {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// The file named by `WS_LIFECYCLE_LOG`, or `None` when unset or unopenable
/// (said on stderr: tracing is not up yet, and a missing log must not stop
/// the server).
pub fn from_env() -> Option<RotatingFile> {
    let path = std::env::var(ENV).ok().filter(|p| !p.trim().is_empty())?;
    match RotatingFile::open(PathBuf::from(&path), MAX_BYTES) {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!("{ENV}={path} could not be opened ({e}); continuing without the lifecycle log");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_rotates_once_full_and_keeps_one_previous_generation() {
        let dir = std::env::temp_dir().join(format!("ws-lifecycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("ws.log");
        let mut log = RotatingFile::open(path.clone(), 10).unwrap();
        log.write_all(b"first-line\n").unwrap();
        log.write_all(b"second\n").unwrap();
        log.write_all(b"third\n").unwrap();
        // The second write found the file full: the first line moved to .1.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\nthird\n");
        assert_eq!(std::fs::read_to_string(dir.join("nested").join("ws.log.1")).unwrap(), "first-line\n");
        // Reopening appends rather than truncating.
        drop(log);
        let mut log = RotatingFile::open(path.clone(), 1_000).unwrap();
        log.write_all(b"fourth\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\nthird\nfourth\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
