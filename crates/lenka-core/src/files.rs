// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Safe reads of local files. `clippy.toml` sends all reads in the workspace through this module.
//!
//! The open of a named pipe waits with no limit, and a symlink can lead out of the data. A file
//! can also be larger than the memory of the process. So each read here refuses a file that is
//! not regular, and each read of a whole file has a size limit.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::Path;

/// The limit of a small settings file, such as `lenka.toml` or `projects.toml`.
pub const SETTINGS_LIMIT: u64 = 1024 * 1024;

/// What a read does with a symlink at the last part of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Links {
    /// Refuse a symlink. Use this for data files, and for a path that another process can change.
    Refuse,
    /// Follow a symlink. Use this for a file of the person, such as a font or a user setting.
    Follow,
}

/// Opens a regular file to read.
///
/// With [`Links::Refuse`] on Unix, the call also compares the open file with the file at the path
/// after the open. So a swap of the path for a symlink during the open fails. Windows has only the
/// check before the open.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidInput`] for a path that is not a regular file, and the error of
/// the system if the open fails.
pub fn open_regular(path: &Path, links: Links) -> io::Result<File> {
    let before = match links {
        Links::Refuse => fs::symlink_metadata(path)?,
        Links::Follow => fs::metadata(path)?,
    };
    if !before.is_file() {
        return Err(not_regular());
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "This is the guard for other reads."
    )]
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }
    if links == Links::Refuse {
        check_same_file(&file, path)?;
    }
    Ok(file)
}

/// Reads a whole regular file.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidData`] for a file above `limit`, and the errors of
/// [`open_regular`].
pub fn read_limited(path: &Path, links: Links, limit: u64) -> io::Result<Vec<u8>> {
    let file = open_regular(path, links)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("The file is larger than the limit of {limit} bytes."),
        ));
    }
    Ok(bytes)
}

/// Reads a whole regular file as UTF-8 text.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidData`] for text that is not valid, and the errors of
/// [`read_limited`].
pub fn read_text_limited(path: &Path, links: Links, limit: u64) -> io::Result<String> {
    String::from_utf8(read_limited(path, links, limit)?)
        .map_err(|_err| io::Error::new(ErrorKind::InvalidData, "The file is not valid UTF-8."))
}

/// Flushes a directory, so that a rename stays after a power failure.
///
/// Windows cannot do this. Some file systems also refuse it, and that is not an error here.
///
/// # Errors
///
/// Returns the error of the system if the open or the flush fails.
pub fn sync_directory(dir: &Path) -> io::Result<()> {
    if !cfg!(unix) {
        return Ok(());
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "The guard is for files, and this is a directory."
    )]
    let handle = File::open(dir)?;
    accept_missing_sync(handle.sync_all())
}

/// Some file systems cannot flush a directory, and this is not a failed write.
fn accept_missing_sync(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(err) if matches!(err.kind(), ErrorKind::Unsupported | ErrorKind::InvalidInput) => {
            Ok(())
        }
        other => other,
    }
}

fn not_regular() -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, "The path is not a regular file.")
}

#[cfg(unix)]
fn check_same_file(file: &File, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if current.is_file() && opened.dev() == current.dev() && opened.ino() == current.ino() {
        Ok(())
    } else {
        Err(io::Error::new(
            ErrorKind::InvalidInput,
            "The path changed during the open.",
        ))
    }
}

#[cfg(not(unix))]
fn check_same_file(_file: &File, _path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regular_file_reads_within_its_limit() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("a");
        fs::write(&file, "four")?;
        assert_eq!(read_text_limited(&file, Links::Refuse, 4)?, "four");
        let large = read_limited(&file, Links::Refuse, 3).map(|_| ());
        assert_eq!(large.map_err(|err| err.kind()), Err(ErrorKind::InvalidData));
        Ok(())
    }

    #[test]
    fn text_that_is_not_utf8_is_invalid_data() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("a");
        fs::write(&file, [0xff, 0xfe])?;
        let result = read_text_limited(&file, Links::Refuse, 4).map(|_| ());
        assert_eq!(
            result.map_err(|err| err.kind()),
            Err(ErrorKind::InvalidData)
        );
        Ok(())
    }

    #[test]
    fn a_directory_is_not_a_regular_file() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        for links in [Links::Refuse, Links::Follow] {
            let result = open_regular(temp.path(), links).map(|_| ());
            assert_eq!(
                result.map_err(|err| err.kind()),
                Err(ErrorKind::InvalidInput)
            );
        }
        Ok(())
    }

    #[test]
    fn a_missing_file_gives_not_found() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let result = open_regular(&temp.path().join("none"), Links::Refuse).map(|_| ());
        assert_eq!(result.map_err(|err| err.kind()), Err(ErrorKind::NotFound));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_follows_only_on_request() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("a");
        let link = temp.path().join("b");
        fs::write(&file, "text")?;
        std::os::unix::fs::symlink(&file, &link)?;
        assert_eq!(read_text_limited(&link, Links::Follow, 9)?, "text");
        let refused = open_regular(&link, Links::Refuse).map(|_| ());
        assert_eq!(
            refused.map_err(|err| err.kind()),
            Err(ErrorKind::InvalidInput)
        );
        Ok(())
    }

    /// The open of a named pipe with no reader waits with no limit, so this test has a time limit.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_with_no_wait() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let pipe = temp.path().join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&pipe)
                .status()?
                .success()
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let kind = open_regular(&pipe, Links::Follow)
                .map(|_| ())
                .map_err(|err| err.kind());
            sender.send(kind).ok();
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|err| io::Error::new(ErrorKind::TimedOut, err))?;
        assert_eq!(result, Err(ErrorKind::InvalidInput));
        Ok(())
    }

    /// An agent can swap a source path during the open. This is the check after the open.
    #[cfg(unix)]
    #[test]
    fn a_path_that_changed_after_the_open_is_refused() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("a");
        let outside = temp.path().join("outside");
        fs::write(&file, "text")?;
        fs::write(&outside, "secret")?;
        let opened = open_regular(&outside, Links::Refuse)?;
        assert_eq!(
            check_same_file(&opened, &file).map_err(|err| err.kind()),
            Err(ErrorKind::InvalidInput)
        );
        assert!(check_same_file(&opened, &outside).is_ok());
        Ok(())
    }

    #[test]
    fn only_a_missing_flush_function_is_accepted() {
        assert!(accept_missing_sync(Ok(())).is_ok());
        assert!(accept_missing_sync(Err(ErrorKind::Unsupported.into())).is_ok());
        assert!(accept_missing_sync(Err(ErrorKind::InvalidInput.into())).is_ok());
        assert!(accept_missing_sync(Err(ErrorKind::PermissionDenied.into())).is_err());
        assert!(accept_missing_sync(Err(io::Error::other("disk error"))).is_err());
    }

    #[test]
    fn a_directory_flush_succeeds() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        sync_directory(temp.path())
    }
}
