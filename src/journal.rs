use crate::{
    Error, Result,
    error::IoContext,
    filesystem::{self, Identity, Stamp},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Prepared,
    Done,
    Undoing,
    Undone,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Move {
        from: PathBuf,
        to: PathBuf,
        stamp: Stamp,
        digest: String,
    },
    Directory {
        path: PathBuf,
        slot: String,
        identity: Identity,
        creating: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub operation: Operation,
    pub state: State,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub root: PathBuf,
    pub root_identity: Identity,
    pub staging: String,
    pub complete: bool,
    pub records: Vec<Record>,
}

pub trait JournalWriter {
    fn write(&mut self, path: &Path, journal: &Journal) -> Result<()>;
}

pub struct DiskJournal;

impl JournalWriter for DiskJournal {
    fn write(&mut self, path: &Path, journal: &Journal) -> Result<()> {
        let data =
            serde_json::to_vec_pretty(journal).map_err(|e| Error::Recovery(e.to_string()))?;
        filesystem::atomic_write(path, &data)
    }
}

pub fn pending(root: &Path) -> PathBuf {
    root.join(".fortify/pending-run.json")
}

pub fn last(root: &Path) -> PathBuf {
    root.join(".fortify/last-run.json")
}

fn single_name(name: &str, prefix: &str) -> bool {
    name.starts_with(prefix)
        && !name.contains(['/', '\\', ':'])
        && name.len() > prefix.len()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn read(path: &Path, root: &Path) -> Result<Option<Journal>> {
    let Some(meta) = filesystem::metadata(path)? else {
        return Ok(None);
    };

    if filesystem::is_link(&meta) || !meta.is_file() {
        return Err(Error::Recovery(format!(
            "journal is not a regular file: {}",
            path.display()
        )));
    }

    let data = fs::read(path).context("read journal")?;
    let journal: Journal = serde_json::from_slice(&data)
        .map_err(|e| Error::Recovery(format!("invalid journal {}: {e}", path.display())))?;

    if journal.version != 1
        || journal.root != root
        || journal.root_identity != filesystem::identity(root)?
    {
        return Err(Error::Recovery(
            "journal version or target identity does not match".into(),
        ));
    }

    if !single_name(&journal.staging, "run-") {
        return Err(Error::Recovery("invalid journal staging name".into()));
    }

    for record in &journal.records {
        match &record.operation {
            Operation::Move {
                from, to, digest, ..
            } => {
                filesystem::validate_relative(from)?;
                filesystem::validate_relative(to)?;
                if from == to
                    || digest.len() != 64
                    || !digest.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(Error::Recovery("invalid move record".into()));
                }
            }

            Operation::Directory { path, slot, .. } => {
                filesystem::validate_relative(path)?;
                if !single_name(slot, "dir-") {
                    return Err(Error::Recovery("invalid directory staging slot".into()));
                }
            }
        }
    }

    Ok(Some(journal))
}

pub fn staging_path(journal: &Journal) -> Result<PathBuf> {
    let base = filesystem::internal_dir(&journal.root)?.join(&journal.staging);
    if let Some(meta) = filesystem::metadata(&base)?
        && (filesystem::is_link(&meta) || !meta.is_dir())
    {
        return Err(Error::Recovery("unsafe journal staging directory".into()));
    }

    Ok(base)
}

pub fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => filesystem::sync_parent(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(format!("remove {}", path.display()), e)),
    }
}

// Staging holds empty directories only. Never recursively delete it.
pub fn cleanup(journal: &Journal) -> Result<()> {
    let base = staging_path(journal)?;

    for record in &journal.records {
        if let Operation::Directory { slot, identity, .. } = &record.operation {
            let path = base.join(slot);
            if let Some(meta) = filesystem::metadata(&path)? {
                if filesystem::is_link(&meta)
                    || !meta.is_dir()
                    || filesystem::identity(&path)? != *identity
                {
                    return Err(Error::Recovery(format!(
                        "staging directory changed: {}",
                        path.display()
                    )));
                }

                fs::remove_dir(&path)
                    .context(format!("remove empty staging directory {}", path.display()))?;
            }
        }
    }

    match fs::remove_dir(&base) {
        Ok(()) => filesystem::sync_parent(&base),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(
            format!("remove empty staging directory {}", base.display()),
            e,
        )),
    }
}
