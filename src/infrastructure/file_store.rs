//! Atomic filesystem boundary for runtime, config, and update writes.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub trait FileStore: Send + Sync + fmt::Debug {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn atomic_write(&self, path: &Path, content: &[u8], sensitive: bool) -> io::Result<()>;
    fn remove_if_exists(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct AtomicFileStore;

impl FileStore for AtomicFileStore {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn atomic_write(&self, path: &Path, content: &[u8], sensitive: bool) -> io::Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temp = temporary_path(path);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temp)?;

        #[cfg(unix)]
        if sensitive {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }

        let result = (|| {
            file.write_all(content)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, path)?;
            if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn remove_if_exists(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("atomic-write");
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "opencode2api-file-store-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn atomic_write_replaces_content_without_temp_leftovers() {
        let root = temp_dir("replace");
        let path = root.join("config.toml");
        let store = AtomicFileStore;
        store.atomic_write(&path, b"first", false).unwrap();
        store.atomic_write(&path, b"second", false).unwrap();
        assert_eq!(store.read(&path).unwrap(), b"second");
        let leftovers = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_write_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_dir("permissions");
        let path = root.join("secret.env");
        AtomicFileStore
            .atomic_write(&path, b"TOKEN=secret", true)
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(root);
    }
}
