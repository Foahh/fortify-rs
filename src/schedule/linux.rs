use super::{Backend, Registration, Spec, render};
use crate::{Error, Result, error::IoContext, filesystem};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub struct Linux {
    directory: PathBuf,
    unit: String,
}

fn systemctl(args: &[&str]) -> Result<Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("run systemctl --user (a systemd user manager is required)")
}

fn checked(args: &[&str]) -> Result<()> {
    let output = systemctl(args)?;
    if !output.status.success() {
        return Err(Error::Scheduler(format!(
            "systemctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(())
}

fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("read {}", path.display()), e)),
    }
}

impl Linux {
    pub fn new() -> Result<Self> {
        Self::named("fortify".into())
    }

    #[cfg(feature = "native-smoke")]
    pub fn isolated(id: &str) -> Result<Self> {
        Self::named(format!("fortify-test-{id}"))
    }

    fn named(unit: String) -> Result<Self> {
        checked(&["show-environment"])?;
        let base = directories::BaseDirs::new()
            .ok_or_else(|| Error::Scheduler("no user configuration directory".into()))?;

        Ok(Self {
            directory: base.config_dir().join("systemd/user"),
            unit,
        })
    }

    fn timer(&self) -> String {
        format!("{}.timer", self.unit)
    }

    fn service(&self) -> String {
        format!("{}.service", self.unit)
    }

    fn put(&self, filename: &str, content: Option<&str>) -> Result<()> {
        let path = self.directory.join(filename);
        if let Some(content) = content {
            fs::create_dir_all(&self.directory).context("create systemd user unit directory")?;
            filesystem::atomic_write(&path, content.as_bytes())
        } else {
            crate::journal::remove_file(&path)
        }
    }
}

impl Backend for Linux {
    fn snapshot(&mut self) -> Result<Registration> {
        checked(&["show-environment"])?;
        let active = systemctl(&["is-active", &self.timer()])?;
        let enabled = systemctl(&["is-enabled", &self.timer()])?;
        if !matches!(active.status.code(), Some(0 | 3 | 4))
            || !matches!(enabled.status.code(), Some(0 | 1 | 3 | 4))
        {
            return Err(Error::Scheduler(format!(
                "cannot query timer: {} {}",
                String::from_utf8_lossy(&active.stderr),
                String::from_utf8_lossy(&enabled.stderr)
            )));
        }

        Ok(Registration {
            primary: read(&self.directory.join(self.service()))?,
            secondary: read(&self.directory.join(self.timer()))?,
            enabled: active.status.success(),
            autostart: enabled.status.success(),
        })
    }

    fn install(&mut self, spec: &Spec) -> Result<()> {
        let (service, timer) = render::systemd(spec)?;
        let timer = timer.replace("Unit=fortify.service", &format!("Unit={}", self.service()));
        let previous = self.snapshot()?;
        if previous.secondary.is_some() || previous.enabled {
            checked(&["stop", &self.timer()])?;
        }

        self.put(&self.service(), Some(&service))?;
        self.put(&self.timer(), Some(&timer))?;
        checked(&["daemon-reload"])?;
        checked(&["enable", "--now", &self.timer()])
    }

    fn restore(&mut self, previous: &Registration) -> Result<()> {
        let current = self.snapshot()?;
        if current.secondary.is_some() || current.enabled || current.autostart {
            checked(&["disable", "--now", &self.timer()])?;
        }

        self.put(&self.service(), previous.primary.as_deref())?;
        self.put(&self.timer(), previous.secondary.as_deref())?;
        checked(&["daemon-reload"])?;
        if previous.autostart {
            checked(&["enable", &self.timer()])?;
        }

        if previous.enabled {
            checked(&["start", &self.timer()])?;
        }

        Ok(())
    }

    fn disable(&mut self) -> Result<()> {
        let current = self.snapshot()?;
        if current.secondary.is_some() || current.enabled || current.autostart {
            checked(&["disable", "--now", &self.timer()])?;
        }

        self.put(&self.service(), None)?;
        self.put(&self.timer(), None)?;
        checked(&["daemon-reload"])
    }
}
