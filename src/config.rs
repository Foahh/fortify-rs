use crate::error::IoContext;
use crate::paths::{self, AppPaths};
use crate::{Error, Result};
use globset::{GlobBuilder, GlobMatcher};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

pub const DEFAULT_CONFIG: &str = include_str!("../config.toml");

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Operations {
    pub sort: bool,
    pub duplicates: bool,
    pub prune: bool,
}

impl Default for Operations {
    fn default() -> Self {
        Self {
            sort: true,
            duplicates: true,
            prune: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(rename = "match")]
    pub patterns: Vec<String>,
    pub folder: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    #[serde(default = "yes")]
    pub sort: bool,
    #[serde(default = "yes")]
    pub duplicates: bool,
    #[serde(default = "yes")]
    pub prune: bool,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub mapping: IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

pub struct Config {
    pub raw: RawConfig,
    pub source: Option<PathBuf>,
    pub destinations: BTreeSet<PathBuf>,
    ignored: Vec<GlobMatcher>,
    exact: HashMap<String, PathBuf>,
    extensions: Vec<(GlobMatcher, PathBuf)>,
    rules: Vec<(String, Vec<GlobMatcher>, PathBuf)>,
}

pub fn compile_glob(pattern: &str) -> Result<GlobMatcher> {
    if pattern.is_empty() {
        return Err(Error::Config("empty glob".into()));
    }

    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| Error::Config(format!("glob {pattern:?}: {e}")))
}

pub fn safe_folder(folder: &str) -> Result<PathBuf> {
    if folder.is_empty() || folder.starts_with(['/', '\\']) {
        return Err(Error::Config(format!(
            "destination must be relative: {folder:?}"
        )));
    }

    let parts: Vec<_> = folder.split(['/', '\\']).collect();

    for part in &parts {
        let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (base.len() == 4
                && (base.starts_with("COM") || base.starts_with("LPT"))
                && matches!(base.as_bytes()[3], b'1'..=b'9'));
        if part.is_empty()
            || *part == "."
            || *part == ".."
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || ":*?\"<>|".contains(c))
            || device
        {
            return Err(Error::Config(format!("unsafe destination: {folder:?}")));
        }
    }

    if parts
        .iter()
        .any(|p| p.eq_ignore_ascii_case(".fortify") || p.eq_ignore_ascii_case("Duplicates"))
    {
        return Err(Error::Config(format!("reserved destination: {folder:?}")));
    }

    Ok(parts.iter().collect())
}

impl Config {
    pub fn parse(text: &str, source: Option<PathBuf>) -> Result<Self> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        if raw.version != 1 {
            return Err(Error::Config(format!(
                "unsupported version {}; expected 1",
                raw.version
            )));
        }

        let mut config = Self {
            raw,
            source,
            destinations: BTreeSet::from([PathBuf::from("Others")]),
            ignored: vec![],
            exact: HashMap::new(),
            extensions: vec![],
            rules: vec![],
        };

        for pattern in &config.raw.ignore {
            config.ignored.push(compile_glob(pattern)?);
        }

        for (folder, extensions) in &config.raw.mapping {
            let folder = safe_folder(folder)?;
            config.destinations.insert(folder.clone());
            if extensions.is_empty() {
                return Err(Error::Config("extension mapping must not be empty".into()));
            }

            for extension in extensions {
                let pattern = extension
                    .strip_prefix('.')
                    .unwrap_or(extension)
                    .to_lowercase();
                if pattern.is_empty() || pattern.contains(['/', '\\']) {
                    return Err(Error::Config(format!(
                        "invalid extension pattern {extension:?}"
                    )));
                }

                let glob = compile_glob(&pattern)?;
                if pattern.contains(['*', '?', '[', '{']) {
                    config.extensions.push((glob, folder.clone()));
                } else if let Some(old) = config.exact.insert(pattern.clone(), folder.clone())
                    && old != folder
                {
                    return Err(Error::Config(format!(
                        "conflicting mapping for {pattern:?}"
                    )));
                }
            }
        }

        let mut names = HashSet::new();

        for rule in &config.raw.rules {
            if rule.name.trim().is_empty()
                || !names.insert(rule.name.clone())
                || rule.patterns.is_empty()
            {
                return Err(Error::Config(format!(
                    "empty or duplicate rule name/patterns: {:?}",
                    rule.name
                )));
            }

            let folder = safe_folder(&rule.folder)?;
            config.destinations.insert(folder.clone());

            let globs = rule
                .patterns
                .iter()
                .map(|p| {
                    if p.contains(['/', '\\']) {
                        Err(Error::Config(format!(
                            "named rule patterns match basenames only: {p:?}"
                        )))
                    } else {
                        compile_glob(p)
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            config.rules.push((rule.name.clone(), globs, folder));
        }

        Ok(config)
    }

    pub fn load(explicit: Option<&Path>, app: &AppPaths) -> Result<Self> {
        let path = explicit
            .map(Path::to_path_buf)
            .or_else(|| app.config.exists().then(|| app.config.clone()));
        if let Some(path) = path {
            let path = paths::absolute(&path)?;
            let text = std::fs::read_to_string(&path)
                .context(format!("read config {}", path.display()))
                .map_err(|e| Error::Config(e.to_string()))?;
            Self::parse(&text, Some(path))
        } else {
            Self::parse(DEFAULT_CONFIG, None)
        }
    }

    pub fn target(&self, explicit: Option<&Path>) -> Result<PathBuf> {
        if let Some(path) = explicit {
            return paths::absolute(path);
        }

        if let Some(path) = &self.raw.directory {
            return paths::absolute(&if path.is_absolute() {
                path.clone()
            } else if let Some(source) = &self.source {
                source.parent().unwrap().join(path)
            } else {
                path.clone()
            });
        }

        paths::downloads().ok_or_else(|| Error::Config("no target directory; use -d <path>".into()))
    }

    pub fn operations(&self) -> Operations {
        Operations {
            sort: self.raw.sort,
            duplicates: self.raw.duplicates,
            prune: self.raw.prune,
        }
    }

    pub fn should_descend(&self, relative: &Path) -> bool {
        self.destinations.iter().any(|d| d.starts_with(relative))
    }

    pub fn ignored(&self, relative: &Path) -> bool {
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let name = relative.file_name().unwrap_or_default();

        self.ignored
            .iter()
            .any(|g| g.is_match(&normalized) || g.is_match(Path::new(name)))
    }

    pub fn destination(&self, relative: &Path) -> (PathBuf, Option<String>) {
        let name = relative.file_name().unwrap_or_default();

        for (rule, globs, folder) in &self.rules {
            if globs.iter().any(|g| g.is_match(Path::new(name))) {
                return (folder.clone(), Some(rule.clone()));
            }
        }

        let ext = relative
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if let Some(folder) = self.exact.get(&ext) {
            return (folder.clone(), None);
        }

        for (glob, folder) in &self.extensions {
            if glob.is_match(&ext) {
                return (folder.clone(), None);
            }
        }

        (PathBuf::from("Others"), None)
    }
}
