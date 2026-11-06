// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The extra checks of `lenka doctor`: network file systems and Git LFS pointer files.

use std::path::Path;

/// The file systems with file locks that lenka cannot trust.
const NETWORK_TYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "ncpfs",
    "afs",
    "ceph",
    "glusterfs",
    "lustre",
    "gpfs",
    "9p",
    "beegfs",
];

/// Reads the type of the mount that holds a path, from the text of `/proc/self/mountinfo`.
///
/// A line has this form, and the mount point is the fifth field:
/// `36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw`.
#[must_use]
pub fn mount_type<'a>(mountinfo: &'a str, path: &Path) -> Option<&'a str> {
    let mut best: Option<(usize, &'a str)> = None;
    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let Some(point) = left.split(' ').nth(4) else {
            continue;
        };
        let Some(kind) = right.split(' ').next() else {
            continue;
        };
        let point = unescape(point);
        if path.starts_with(&point) && best.is_none_or(|(len, _)| point.len() >= len) {
            best = Some((point.len(), kind));
        }
    }
    best.map(|(_, kind)| kind)
}

/// Decodes the octal escapes of a mount point, such as `\040` for a space.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let code = (bytes[index] == b'\\')
            .then(|| field.get(index + 1..index + 4))
            .flatten()
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        if let Some(code) = code {
            out.push(code);
            index += 4;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Tells if a mount type is a network file system.
#[must_use]
pub fn is_network_type(kind: &str) -> bool {
    NETWORK_TYPES.contains(&kind) || kind == "fuse.sshfs" || kind == "fuse.rclone"
}

/// Returns the network mount type of a path. Only Linux can show this.
#[must_use]
pub fn network_mount(path: &Path) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let real = std::fs::canonicalize(path).ok()?;
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    mount_type(&text, &real)
        .filter(|kind| is_network_type(kind))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
40 22 0:40 / /mnt/nas rw,relatime shared:2 - nfs4 server:/export rw
41 40 8:2 / /mnt/nas/local rw - ext4 /dev/sdb1 rw
42 22 0:41 / /mnt/my\\040share rw - cifs //host/share rw
43 22 0:42 / /mnt/ssh rw,nosuid - fuse.sshfs me@host:/ rw
bad line
";

    #[test]
    fn the_longest_mount_point_wins() {
        let kind = |path: &str| mount_type(SAMPLE, Path::new(path));
        assert_eq!(kind("/home/me"), Some("ext4"));
        assert_eq!(kind("/mnt/nas/data"), Some("nfs4"));
        assert_eq!(kind("/mnt/nas/local/data"), Some("ext4"));
        assert_eq!(kind("/mnt/nasty"), Some("ext4"));
        assert_eq!(kind("/mnt/my share/data"), Some("cifs"));
        assert_eq!(kind("/mnt/ssh/x"), Some("fuse.sshfs"));
    }

    #[test]
    fn network_types() {
        assert!(is_network_type("nfs4"));
        assert!(is_network_type("fuse.sshfs"));
        assert!(!is_network_type("ext4"));
        assert!(!is_network_type("btrfs"));
    }
}
