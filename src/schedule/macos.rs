use super::{Backend, Registration, Spec, render};
use crate::{Error, Result, error::IoContext, filesystem};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

pub struct Macos {
    path: PathBuf,
    domain: String,
    label: String,
}

fn launchctl(args: &[&str]) -> Result<Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .context("run launchctl")
}

fn checked(args: &[&str]) -> Result<()> {
    let output = launchctl(args)?;
    if !output.status.success() {
        return Err(Error::Scheduler(format!(
            "launchctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(())
}

impl Macos {
    pub fn new() -> Result<Self> {
        Self::named("com.fortify.organize".into())
    }

    #[cfg(feature = "native-smoke")]
    pub fn isolated(id: &str) -> Result<Self> {
        Self::named(format!("com.fortify.test.{id}"))
    }

    fn named(label: String) -> Result<Self> {
        let user = directories::BaseDirs::new()
            .ok_or_else(|| Error::Scheduler("no home directory".into()))?;
        let domain = format!("gui/{}", rustix::process::getuid().as_raw());
        checked(&["print", &domain])?;

        Ok(Self {
            path: user
                .home_dir()
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist")),
            domain,
            label,
        })
    }

    fn service(&self) -> String {
        format!("{}/{}", self.domain, self.label)
    }

    fn unload(&mut self) -> Result<()> {
        if self.snapshot()?.enabled {
            checked(&["bootout", &self.service()])?;
        }

        Ok(())
    }

    fn write(&self, content: &str) -> Result<()> {
        plist::Value::from_reader_xml(content.as_bytes())
            .map_err(|e| Error::Scheduler(e.to_string()))?;
        fs::create_dir_all(self.path.parent().unwrap()).context("create LaunchAgents directory")?;
        filesystem::atomic_write(&self.path, content.as_bytes())
    }
}

impl Backend for Macos {
    fn snapshot(&mut self) -> Result<Registration> {
        checked(&["print", &self.domain])?;
        let primary = match fs::read_to_string(&self.path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::io("read LaunchAgent", e)),
        };

        let status = launchctl(&["print", &self.service()])?;
        if !status.status.success() && !matches!(status.status.code(), Some(113 | 3)) {
            return Err(Error::Scheduler(format!(
                "query LaunchAgent: {}",
                String::from_utf8_lossy(&status.stderr)
            )));
        }

        let overrides = launchctl(&["print-disabled", &self.domain])?;
        if !overrides.status.success() {
            return Err(Error::Scheduler(
                "cannot query launchd disabled overrides".into(),
            ));
        }

        let label = format!("\"{}\"", self.label);
        let disabled = String::from_utf8_lossy(&overrides.stdout)
            .lines()
            .any(|line| line.trim().starts_with(&label) && line.contains("=> true"));

        Ok(Registration {
            primary,
            secondary: None,
            enabled: status.status.success(),
            autostart: !disabled,
        })
    }

    fn install(&mut self, spec: &Spec) -> Result<()> {
        let content = render::launchd(spec, &self.label)?;
        self.unload()?;
        self.write(&content)?;
        checked(&["enable", &self.service()])?;
        checked(&["bootstrap", &self.domain, crate::paths::utf8(&self.path)?])
    }

    fn restore(&mut self, previous: &Registration) -> Result<()> {
        self.unload()?;
        checked(&[
            if previous.autostart {
                "enable"
            } else {
                "disable"
            },
            &self.service(),
        ])?;
        if let Some(content) = &previous.primary {
            self.write(content)?;
            if previous.enabled {
                checked(&["bootstrap", &self.domain, crate::paths::utf8(&self.path)?])?;
            }
        } else {
            crate::journal::remove_file(&self.path)?;
        }

        Ok(())
    }

    fn disable(&mut self) -> Result<()> {
        self.unload()?;
        crate::journal::remove_file(&self.path)
    }
}
