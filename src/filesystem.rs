use crate::error::IoContext;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

pub const INTERNAL: &str = ".fortify";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub volume: u64,
    pub index: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub identity: Identity,
    pub size: u64,
    pub modified_ns: u64,
}

pub fn is_link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        meta.file_type().is_symlink() || meta.file_attributes() & 0x400 != 0
    }

    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

pub fn metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("inspect {}", path.display()), e)),
    }
}

pub fn identity(path: &Path) -> Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let meta = fs::symlink_metadata(path).context(format!("identify {}", path.display()))?;

        Ok(Identity {
            volume: meta.dev(),
            index: meta.ino(),
        })
    }

    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
        };

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(0x02000000 | 0x00200000)
            .open(path)
            .context(format!("identify {}", path.display()))?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();

        // SAFETY: file owns a live handle; info is a valid writable structure.
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|e| Error::Operation(format!("identify {}: {e}", path.display())))?;

        Ok(Identity {
            volume: info.dwVolumeSerialNumber as u64,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        })
    }
}

pub fn stamp(path: &Path) -> Result<Stamp> {
    let meta = fs::symlink_metadata(path).context(format!("inspect {}", path.display()))?;
    if is_link(&meta) || !meta.is_file() {
        return Err(Error::Operation(format!(
            "not a regular file: {}",
            path.display()
        )));
    }

    let modified_ns = meta
        .modified()
        .context("read modification time")?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Operation("pre-epoch modification time is unsupported".into()))?
        .as_nanos()
        .try_into()
        .map_err(|_| Error::Operation("modification time exceeds supported range".into()))?;

    Ok(Stamp {
        identity: identity(path)?,
        size: meta.len(),
        modified_ns,
    })
}

pub fn verify_stamp(path: &Path, expected: &Stamp) -> Result<()> {
    if stamp(path)? != *expected {
        return Err(Error::Operation(format!(
            "file changed since planning: {}",
            path.display()
        )));
    }

    Ok(())
}

pub fn target(path: &Path) -> Result<PathBuf> {
    let meta = fs::symlink_metadata(path).context(format!("open target {}", path.display()))?;
    if is_link(&meta) || !meta.is_dir() {
        return Err(Error::Operation(format!(
            "target must be a real directory: {}",
            path.display()
        )));
    }

    let root = fs::canonicalize(path).context("canonicalize target")?;
    crate::paths::utf8(&root)?;

    Ok(root)
}

pub fn validate_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(Error::Recovery(format!(
            "invalid relative path {}",
            path.display()
        )));
    }

    crate::paths::utf8(path)?;
    if path.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(INTERNAL)
    }) {
        return Err(Error::Recovery(
            "operation targets reserved .fortify directory".into(),
        ));
    }

    Ok(())
}

pub fn checked_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    validate_relative(relative)?;
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.components().collect();

    for (i, component) in components.iter().enumerate() {
        current.push(component);
        if let Some(meta) = metadata(&current)? {
            if is_link(&meta) || (i + 1 < components.len() && !meta.is_dir()) {
                return Err(Error::Operation(format!(
                    "unsafe path component: {}",
                    current.display()
                )));
            }

            if !fs::canonicalize(&current)
                .context("check containment")?
                .starts_with(root)
            {
                return Err(Error::Operation(format!(
                    "path escapes target: {}",
                    current.display()
                )));
            }
        }
    }

    Ok(current)
}

pub fn internal_dir(root: &Path) -> Result<PathBuf> {
    let path = root.join(INTERNAL);

    match metadata(&path)? {
        Some(meta) if is_link(&meta) || !meta.is_dir() => {
            return Err(Error::Recovery(".fortify must be a real directory".into()));
        }

        Some(_) => {}
        None => fs::create_dir(&path).context("create journal directory")?,
    }

    Ok(path)
}

pub fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).context(format!("read {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];

    loop {
        let n = file.read(&mut buffer).context("hash file")?;
        if n == 0 {
            break;
        }

        hasher.update(&buffer[..n]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

pub fn equal_files(a: &Path, b: &Path) -> Result<bool> {
    let mut a = File::open(a).context("open duplicate candidate")?;
    let mut b = File::open(b).context("open duplicate candidate")?;
    let mut left = [0u8; 65536];
    let mut right = [0u8; 65536];

    loop {
        let n = read_chunk(&mut a, &mut left)?;
        let m = read_chunk(&mut b, &mut right)?;
        if n != m || left[..n] != right[..m] {
            return Ok(false);
        }

        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_chunk(file: &mut File, buffer: &mut [u8]) -> Result<usize> {
    let mut n = 0;

    while n < buffer.len() {
        let count = file
            .read(&mut buffer[n..])
            .context("compare file contents")?;
        if count == 0 {
            break;
        }

        n += count;
    }

    Ok(n)
}

pub fn rename_no_replace(from: &Path, to: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};

        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(|e| {
            Error::io(
                format!(
                    "move {} to {} (no replacement)",
                    from.display(),
                    to.display()
                ),
                e.into(),
            )
        })?;
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW},
            core::PCWSTR,
        };

        let from_w: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to_w: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();

        // SAFETY: buffers are live, NUL-terminated UTF-16 strings.
        // Flags deliberately exclude replacement and cross-volume copying.
        unsafe {
            MoveFileExW(
                PCWSTR(from_w.as_ptr()),
                PCWSTR(to_w.as_ptr()),
                MOVE_FILE_FLAGS(0),
            )
        }
        .map_err(|e| {
            Error::Operation(format!(
                "move {} to {} (no replacement): {e}",
                from.display(),
                to.display()
            ))
        })?;
    }

    sync_parent(from)?;
    if from.parent() != to.parent() {
        sync_parent(to)?;
    }

    Ok(())
}

pub fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(
            path.parent()
                .ok_or_else(|| Error::Operation("path has no parent".into()))?,
        )
        .context("open parent directory")?
        .sync_all()
        .context("flush parent directory")?;
    }

    #[cfg(windows)]
    {
        let _ = path;
    }

    Ok(())
}

pub fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(meta) = metadata(path)?
        && (is_link(&meta) || !meta.is_file())
    {
        return Err(Error::Operation(format!(
            "refuse replacing non-regular file {}",
            path.display()
        )));
    }

    let parent = path
        .parent()
        .ok_or_else(|| Error::Operation("path has no parent".into()))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("create atomic temporary file")?;
    temporary
        .write_all(content)
        .context("write temporary file")?;
    temporary
        .as_file()
        .sync_all()
        .context("flush temporary file")?;
    temporary
        .persist(path)
        .map_err(|e| Error::io(format!("replace {}", path.display()), e.error))?;
    sync_parent(path)
}

pub fn create_new(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .context(format!("create {}", path.display()))?;
    file.write_all(content).context("write new file")?;
    file.sync_all().context("flush new file")?;
    sync_parent(path)
}

pub struct TargetLock {
    _file: File,
}

impl TargetLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        let dir = internal_dir(root)?;
        let path = dir.join("apply.lock");
        if let Some(meta) = metadata(&path)?
            && (is_link(&meta) || !meta.is_file())
        {
            return Err(Error::Recovery("unsafe lock file".into()));
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .context("open target lock")?;

        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(Error::Locked(root.to_path_buf())),
            Err(std::fs::TryLockError::Error(e)) => Err(Error::io("lock target", e)),
        }
    }
}
