use crate::{
    Error, Result,
    config::{Config, Operations},
    error::IoContext,
    filesystem::{self, Stamp},
    scan::{self, Snapshot},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Move {
        from: PathBuf,
        to: PathBuf,
        reason: String,
        stamp: Stamp,
        digest: String,
    },
    RemoveDir {
        path: PathBuf,
        identity: filesystem::Identity,
    },
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub target: PathBuf,
    pub actions: Vec<Action>,
}

pub fn build(root: &Path, config: &Config, operations: Operations) -> Result<Plan> {
    let snapshot = scan::scan(root, config)?;
    build_snapshot(root, config, operations, &snapshot)
}

fn build_snapshot(
    root: &Path,
    config: &Config,
    operations: Operations,
    snapshot: &Snapshot,
) -> Result<Plan> {
    let mut duplicates = HashSet::new();
    let mut digests = HashMap::new();
    if operations.duplicates {
        let mut buckets: BTreeMap<u64, Vec<usize>> = BTreeMap::new();

        for (i, entry) in snapshot.files.iter().enumerate() {
            buckets.entry(entry.stamp.size).or_default().push(i);
        }

        for bucket in buckets.values().filter(|b| b.len() > 1) {
            let mut seen: HashMap<String, Vec<usize>> = HashMap::new();

            for &i in bucket {
                let entry = &snapshot.files[i];
                let path = root.join(&entry.relative);
                filesystem::verify_stamp(&path, &entry.stamp)?;
                let digest = filesystem::hash(&path)?;
                filesystem::verify_stamp(&path, &entry.stamp)?;
                let mut duplicate = false;

                for &prior in seen.get(&digest).into_iter().flatten() {
                    let keeper = &snapshot.files[prior];
                    let keeper_path = root.join(&keeper.relative);
                    filesystem::verify_stamp(&keeper_path, &keeper.stamp)?;
                    if filesystem::equal_files(&path, &keeper_path)? {
                        duplicate = true;
                    }

                    filesystem::verify_stamp(&keeper_path, &keeper.stamp)?;
                    if duplicate {
                        break;
                    }
                }

                filesystem::verify_stamp(&path, &entry.stamp)?;
                if duplicate {
                    duplicates.insert(i);
                } else {
                    seen.entry(digest.clone()).or_default().push(i);
                }

                digests.insert(i, digest);
            }
        }
    }

    let mut actions = vec![];
    let mut reserved = BTreeSet::new();
    let mut remaining = BTreeSet::new();
    let mut vacated = BTreeSet::new();

    for (i, entry) in snapshot.files.iter().enumerate() {
        let (folder, reason) = if duplicates.contains(&i) {
            (PathBuf::from("Duplicates"), "duplicate".to_owned())
        } else {
            let (folder, rule) = config.destination(&entry.relative);
            if !operations.sort
                || entry
                    .relative
                    .parent()
                    .is_some_and(|p| p.starts_with(&folder))
            {
                remaining.insert(entry.relative.clone());
                continue;
            }

            (
                folder,
                rule.map_or_else(|| "sort".to_owned(), |n| format!("sort[{n}]")),
            )
        };

        let dest = unique_path(root, &folder, &entry.relative, &reserved)?;
        reserved.insert(path_key(&dest));
        remaining.insert(dest.clone());
        vacated.insert(entry.relative.clone());

        let path = root.join(&entry.relative);
        filesystem::verify_stamp(&path, &entry.stamp)?;
        let digest = match digests.remove(&i) {
            Some(d) => d,
            None => filesystem::hash(&path)?,
        };

        filesystem::verify_stamp(&path, &entry.stamp)?;

        actions.push(Action::Move {
            from: entry.relative.clone(),
            to: dest,
            reason,
            stamp: entry.stamp.clone(),
            digest,
        });
    }

    if operations.prune {
        let mut removed = BTreeSet::new();

        for directory in &snapshot.directories {
            if remaining.iter().any(|p| p.starts_with(directory)) {
                continue;
            }

            let mut empty = true;

            for entry in fs::read_dir(root.join(directory)).context("inspect pruning candidate")? {
                let entry = entry.context("inspect pruning entry")?;
                let rel = directory.join(entry.file_name());
                if !vacated.contains(&rel) && !removed.contains(&rel) {
                    empty = false;
                    break;
                }
            }

            if empty {
                removed.insert(directory.clone());
                actions.push(Action::RemoveDir {
                    path: directory.clone(),
                    identity: filesystem::identity(&root.join(directory))?,
                });
            }
        }
    }

    Ok(Plan {
        target: root.to_path_buf(),
        actions,
    })
}

fn path_key(path: &Path) -> String {
    let s = path.to_string_lossy().into_owned();
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s
    }
}

fn unique_path(
    root: &Path,
    folder: &Path,
    source: &Path,
    reserved: &BTreeSet<String>,
) -> Result<PathBuf> {
    let name = source
        .file_name()
        .ok_or_else(|| Error::Operation("file has no basename".into()))?;
    let stem = source.file_stem().unwrap_or(name).to_string_lossy();
    let ext = source.extension().map(|s| s.to_string_lossy());

    for n in 0u64.. {
        let name = if n == 0 {
            name.to_os_string()
        } else if let Some(ext) = &ext {
            format!("{stem}_{n}.{ext}").into()
        } else {
            format!("{stem}_{n}").into()
        };

        let rel = folder.join(name);
        let candidate = filesystem::checked_path(root, &rel)?;
        if !reserved.contains(&path_key(&rel)) && filesystem::metadata(&candidate)?.is_none() {
            return Ok(rel);
        }
    }

    unreachable!()
}
