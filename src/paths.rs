use crate::error::IoContext;
use crate::{Error, Result};
use directories::{ProjectDirs, UserDirs};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "fortify").ok_or_else(|| {
            Error::Config("cannot locate the user configuration directory".into())
        })?;

        Ok(Self {
            config: dirs.config_dir().join("config.toml"),
            data: dirs
                .state_dir()
                .unwrap_or(dirs.data_local_dir())
                .to_path_buf(),
        })
    }

    pub fn schedule(&self) -> PathBuf {
        self.data.join("schedule.json")
    }

    pub fn log(&self) -> PathBuf {
        self.data.join("fortify.log")
    }
}

pub fn downloads() -> Option<PathBuf> {
    UserDirs::new()?
        .download_dir()
        .filter(|p| p.is_dir())
        .map(Path::to_path_buf)
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).context(format!("resolve {}", path.display()))
}

pub fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Operation(format!(
            "path is not valid Unicode and cannot be journaled safely: {}",
            path.display()
        ))
    })
}
