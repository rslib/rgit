use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use toml_edit::{DocumentMut, Item, Table};

use crate::error::Error;
use crate::settings::RepoDirs;

const LOCK_RETRY: Duration = Duration::from_millis(10);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_owned(),
        source,
    }
}

fn lock(path: &Path) -> Result<LockGuard, Error> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let lock_path = PathBuf::from(name);
    let start = Instant::now();
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Ok(LockGuard(lock_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if start.elapsed() >= LOCK_TIMEOUT {
                    return Err(Error::Lock(lock_path));
                }
                std::thread::sleep(LOCK_RETRY);
            }
            Err(error) => return Err(io(&lock_path)(error)),
        }
    }
}

/// Edit a TOML file in place, keeping comments and order.
///
/// Holds `<path>.lock` for the whole edit and waits up to 5 s for it. A missing
/// file is an empty document. If `f` fails nothing is written.
pub fn edit_toml<E>(
    path: &Path,
    f: impl FnOnce(&mut DocumentMut) -> Result<(), E>,
) -> Result<(), Error>
where
    E: std::error::Error + Send + Sync + 'static,
{
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    let _guard = lock(path)?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(io(path)(error)),
    };
    let mut doc = text.parse::<DocumentMut>().map_err(|error| Error::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    f(&mut doc).map_err(|error| Error::Edit(Box::new(error)))?;
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    std::fs::write(&tmp, doc.to_string()).map_err(io(&tmp))?;
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        io(path)(error)
    })
}

/// Set a dotted key in the private repo file, creating tables as needed.
pub fn set_private(
    repo: &RepoDirs,
    key: &str,
    value: impl Into<toml_edit::Value>,
) -> Result<(), Error> {
    let segments: Vec<&str> = key.split('.').collect();
    let Some((last, parents)) = segments.split_last() else {
        return Err(Error::BadKey(key.to_owned()));
    };
    if segments.iter().any(|s| s.is_empty()) {
        return Err(Error::BadKey(key.to_owned()));
    }
    let value = value.into();
    edit_toml(&repo.private_file(), |doc| {
        let mut table = doc.as_table_mut();
        for segment in parents {
            let item = table.entry(segment).or_insert_with(|| {
                let mut created = Table::new();
                created.set_implicit(true);
                Item::Table(created)
            });
            table = item
                .as_table_mut()
                .ok_or_else(|| Error::BadKey(key.to_owned()))?;
        }
        table.insert(last, Item::Value(value));
        Ok::<(), Error>(())
    })
}

/// Read a TOML file into a table. A missing file is an empty table.
pub fn read_toml(path: &Path) -> Result<toml::Table, Error> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(error) => return Err(io(path)(error)),
    };
    toml::from_str(&text).map_err(|error| Error::Parse {
        path: path.to_owned(),
        message: error.message().to_owned(),
    })
}
