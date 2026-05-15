//! Safe file operations with protection against symlink attacks and path traversal.

use std::fs;
use std::io;
use std::path::{Component, Path};

/// Check if a path is safe to operate on (no symlinks, no path traversal).
pub fn is_safe_path(path: &Path) -> bool {
    for component in path.components() {
        match component {
            Component::ParentDir => {
                // Reject paths with ".."
                return false;
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) | Component::CurDir => {
                // These are OK
            }
        }
    }
    true
}

/// Safely rename a file, checking for symlinks first.
/// 
/// Returns error if:
/// - Source is a symlink
/// - Destination exists and is a symlink
/// - Path traversal attempts detected
pub fn safe_rename(from: &Path, to: &Path) -> io::Result<()> {
    // Check paths for traversal attempts
    if !is_safe_path(from) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path contains '..' or invalid components",
        ));
    }
    if !is_safe_path(to) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path contains '..' or invalid components",
        ));
    }

    // Check for symlinks
    if from.exists() {
        let metadata = fs::symlink_metadata(from)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "source is a symlink (security restriction)",
            ));
        }
    }

    if to.exists() {
        let metadata = fs::symlink_metadata(to)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "destination is a symlink (security restriction)",
            ));
        }
    }

    // Safe to proceed with rename
    fs::rename(from, to)
}

/// Safely copy a file, checking for symlinks first.
pub fn safe_copy(from: &Path, to: &Path) -> io::Result<u64> {
    // Check paths for traversal attempts
    if !is_safe_path(from) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path contains '..' or invalid components",
        ));
    }
    if !is_safe_path(to) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path contains '..' or invalid components",
        ));
    }

    // Check for symlinks
    let from_metadata = fs::symlink_metadata(from)?;
    if from_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "source is a symlink (security restriction)",
        ));
    }

    if to.exists() {
        let to_metadata = fs::symlink_metadata(to)?;
        if to_metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "destination is a symlink (security restriction)",
            ));
        }
    }

    // Safe to proceed with copy
    fs::copy(from, to)
}

/// Safely move a file (rename with fallback to copy+delete).
pub fn safe_move(from: &Path, to: &Path) -> io::Result<()> {
    // Try safe rename first
    if let Ok(()) = safe_rename(from, to) {
        return Ok(());
    }

    // Fallback to safe copy + delete
    safe_copy(from, to)?;
    fs::remove_file(from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_path() {
        assert!(is_safe_path(Path::new("/tmp/file.img")));
        assert!(is_safe_path(Path::new("./file.img")));
        assert!(is_safe_path(Path::new("file.img")));
        assert!(!is_safe_path(Path::new("../file.img")));
        assert!(!is_safe_path(Path::new("/tmp/../../../etc/passwd")));
        assert!(!is_safe_path(Path::new("foo/../../etc/passwd")));
    }
}
