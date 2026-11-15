// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::prime::tests::Temp;
use std::os::unix::fs::PermissionsExt as _;

fn backup_at(
    path: &Path,
    stamp: u64,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<PathBuf, String> {
    let directory = Directory::open(path.parent().unwrap()).map_err(|error| error.to_string())?;
    let copy = new_backup(&directory, path.file_name().unwrap(), stamp, fill)?;
    Ok(directory.path.join(copy.name))
}

fn write_backup(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let mode = std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    backup_at(path, stamp, |file| {
        file.write_all(bytes)?;
        ordered(file)?;
        file.set_permissions(mode)
    })
}

#[test]
fn original_backup_collisions_and_write_failures_leave_other_bytes_untouched() {
    let home = Temp::new().unwrap();
    let settings = home.path().join("settings.json");
    std::fs::write(&settings, b"original").unwrap();
    let first = settings.with_file_name("settings.json.bak-42");
    let second = settings.with_file_name("settings.json.bak-42-1");
    std::fs::write(&first, b"belongs to first").unwrap();
    std::fs::write(&second, b"belongs to second").unwrap();
    let third = backup_at(&settings, 42, |file| file.write_all(b"new backup")).unwrap();
    assert_eq!(third, settings.with_file_name("settings.json.bak-42-2"));
    assert_eq!(std::fs::read(&first).unwrap(), b"belongs to first");
    assert_eq!(std::fs::read(&second).unwrap(), b"belongs to second");
    assert_eq!(std::fs::read(third).unwrap(), b"new backup");
    assert!(
        backup_at(&settings, 43, |file| {
            file.write_all(b"partial")?;
            Err(std::io::Error::other("controlled write failure"))
        })
        .is_err()
    );
    assert!(!settings.with_file_name("settings.json.bak-43").exists());
    assert_eq!(std::fs::read(&settings).unwrap(), b"original");
}

#[test]
fn the_backup_preserves_the_original_mode() {
    let home = Temp::new().unwrap();
    let settings = home.path().join("settings.json");
    std::fs::write(&settings, b"original").unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o640)).unwrap();
    let first = write_backup(&settings, b"original").unwrap();
    let second = write_backup(&settings, b"second").unwrap();
    assert_ne!(first, second);
    assert_eq!(std::fs::metadata(first).unwrap().mode() & 0o777, 0o640);
    assert_eq!(std::fs::metadata(second).unwrap().mode() & 0o777, 0o640);
}

#[cfg(target_os = "linux")]
#[test]
fn the_backup_preserves_native_non_utf8_paths_where_the_filesystem_accepts_them() {
    use std::os::unix::ffi::OsStringExt as _;
    let home = Temp::new().unwrap();
    let settings = home
        .path()
        .join(std::ffi::OsString::from_vec(b"settings-\xff.json".to_vec()));
    std::fs::write(&settings, b"original").unwrap();
    let backup = write_backup(&settings, b"original").unwrap();
    assert_eq!(std::fs::read(backup).unwrap(), b"original");
}
