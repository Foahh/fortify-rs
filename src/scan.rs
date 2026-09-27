use crate::{
    Result,
    config::Config,
    error::IoContext,
    filesystem::{self, Stamp},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub relative: PathBuf,
    pub stamp: Stamp,
}

#[derive(Debug)]
pub struct Snapshot {
    pub files: Vec<Entry>,
    pub directories: Vec<PathBuf>,
}

pub fn scan(root: &Path, config: &Config) -> Result<Snapshot> {
    let mut result = Snapshot {
        files: vec![],
        directories: vec![],
    };

    let config_path = config
        .source
        .as_ref()
        .and_then(|p| fs::canonicalize(p).ok());
    visit(
        root,
        Path::new(""),
        config,
        config_path.as_deref(),
        &mut result,
    )?;

    result.files.sort_by(|a, b| {
        b.stamp
            .modified_ns
            .cmp(&a.stamp.modified_ns)
            .then_with(|| a.relative.cmp(&b.relative))
    });

    result.directories.sort_by(|a, b| {
        b.components()
            .count()
            .cmp(&a.components().count())
            .then_with(|| a.cmp(b))
    });

    Ok(result)
}

fn visit(
    root: &Path,
    relative: &Path,
    config: &Config,
    config_path: Option<&Path>,
    result: &mut Snapshot,
) -> Result<()> {
    let entries = fs::read_dir(root.join(relative)).context("scan directory")?;

    for entry in entries {
        let entry = entry.context("read directory entry")?;
        let name = entry.file_name();
        if name.eq_ignore_ascii_case(".fortify") || name.eq_ignore_ascii_case("Duplicates") {
            continue;
        }

        let rel = relative.join(name);
        crate::paths::utf8(&rel)?;
        if config.ignored(&rel) || config_path.is_some_and(|p| p == root.join(&rel)) {
            continue;
        }

        let Some(meta) = filesystem::metadata(&root.join(&rel))? else {
            continue;
        };

        if filesystem::is_link(&meta) {
            continue;
        }

        let path = filesystem::checked_path(root, &rel)?;
        if meta.is_file() {
            result.files.push(Entry {
                relative: rel,
                stamp: filesystem::stamp(&path)?,
            });
        } else if meta.is_dir() && config.should_descend(&rel) {
            result.directories.push(rel.clone());
            visit(root, &rel, config, config_path, result)?;
        }
    }

    Ok(())
}
