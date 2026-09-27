use crate::{
    Error, Result,
    error::IoContext,
    filesystem,
    paths::{self, AppPaths},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;
pub mod render;

#[cfg(windows)]
mod windows;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Timing {
    Every { seconds: u32 },
    Daily { hour: u8, minute: u8 },
}

impl Timing {
    pub fn every(input: &str) -> Result<Self> {
        let invalid = || {
            Error::Config("interval must be Nm, Nh, or Nd, from 1 minute through 31 days".into())
        };

        if input.len() < 2 || !input.is_ascii() {
            return Err(invalid());
        }

        let (digits, unit) = input.split_at(input.len() - 1);
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }

        let n: u32 = digits.parse().map_err(|_| invalid())?;
        let factor = match unit {
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            _ => return Err(invalid()),
        };

        let seconds = n
            .checked_mul(factor)
            .filter(|s| (60..=31 * 86400).contains(s))
            .ok_or_else(invalid)?;

        Ok(Self::Every { seconds })
    }

    pub fn daily(input: &str) -> Result<Self> {
        let invalid =
            || Error::Config("daily time must be HH:MM in local time (00:00 through 23:59)".into());
        if input.len() != 5
            || !input.is_ascii()
            || &input[2..3] != ":"
            || !input
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 2 || b.is_ascii_digit())
        {
            return Err(invalid());
        }

        let hour: u8 = input[..2].parse().map_err(|_| invalid())?;
        let minute: u8 = input[3..].parse().map_err(|_| invalid())?;
        if hour > 23 || minute > 59 {
            return Err(invalid());
        }

        Ok(Self::Daily { hour, minute })
    }
}

impl fmt::Display for Timing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Every { seconds } => write!(f, "every {seconds} seconds"),
            Self::Daily { hour, minute } => write!(f, "daily at {hour:02}:{minute:02} local time"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub version: u32,
    pub runner: PathBuf,
    pub config: PathBuf,
    pub directory: PathBuf,
    pub log: PathBuf,
    pub timing: Timing,
}

impl Spec {
    pub fn new(config: &Path, directory: &Path, app: &AppPaths, timing: Timing) -> Result<Self> {
        let runner = std::env::current_exe()
            .context("locate executable")?
            .with_file_name(if cfg!(windows) {
                "fortify-runner.exe"
            } else {
                "fortify-runner"
            });
        if !runner.is_file() {
            return Err(Error::Scheduler(
                "fortify-runner must be installed next to fortify; build/install both binaries"
                    .into(),
            ));
        }

        Ok(Self {
            version: 1,
            runner: fs::canonicalize(runner).context("resolve runner")?,
            config: fs::canonicalize(config).context("resolve saved configuration")?,
            directory: filesystem::target(directory)?,
            log: paths::absolute(&app.log())?,
            timing,
        })
    }

    pub fn args(&self) -> Result<Vec<String>> {
        Ok(vec![
            "--config".into(),
            paths::utf8(&self.config)?.into(),
            "--directory".into(),
            paths::utf8(&self.directory)?.into(),
            "--log".into(),
            paths::utf8(&self.log)?.into(),
        ])
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registration {
    pub primary: Option<String>,
    pub secondary: Option<String>,
    pub enabled: bool,
    pub autostart: bool,
}

pub trait Backend {
    fn snapshot(&mut self) -> Result<Registration>;
    fn install(&mut self, spec: &Spec) -> Result<()>;
    fn restore(&mut self, previous: &Registration) -> Result<()>;
    fn disable(&mut self) -> Result<()>;
}

pub fn native() -> Result<Box<dyn Backend>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::Windows::new()?))
    }

    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::Linux::new()?))
    }

    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::Macos::new()?))
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Err(Error::Scheduler("unsupported native scheduler".into()))
    }
}

fn lock(app: &AppPaths) -> Result<File> {
    fs::create_dir_all(&app.data).context("create scheduler state directory")?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(app.data.join("schedule.lock"))
        .context("open scheduler lock")?;
    file.try_lock()
        .map_err(|e| Error::Scheduler(format!("cannot acquire scheduler lock: {e}")))?;

    Ok(file)
}

pub fn read_spec(app: &AppPaths) -> Result<Option<Spec>> {
    match fs::read(app.schedule()) {
        Ok(bytes) => {
            let spec: Spec = serde_json::from_slice(&bytes)
                .map_err(|e| Error::Scheduler(format!("invalid saved schedule: {e}")))?;
            if spec.version != 1 {
                return Err(Error::Scheduler(
                    "unsupported schedule state version".into(),
                ));
            }

            Ok(Some(spec))
        }

        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io("read saved schedule", e)),
    }
}

pub fn enable(app: &AppPaths, backend: &mut dyn Backend, spec: &Spec) -> Result<()> {
    let _lock = lock(app)?;
    read_spec(app)?;
    let previous = backend.snapshot()?;
    let outcome = (|| {
        backend.install(spec)?;
        let data = serde_json::to_vec_pretty(spec).map_err(|e| Error::Scheduler(e.to_string()))?;
        filesystem::atomic_write(&app.schedule(), &data)
    })();
    rollback_on_error(backend, &previous, outcome)
}

pub fn disable(app: &AppPaths, backend: &mut dyn Backend) -> Result<()> {
    let _lock = lock(app)?;
    let previous = backend.snapshot()?;
    let result = backend
        .disable()
        .and_then(|()| crate::journal::remove_file(&app.schedule()));
    rollback_on_error(backend, &previous, result)
}

fn rollback_on_error(
    backend: &mut dyn Backend,
    previous: &Registration,
    outcome: Result<()>,
) -> Result<()> {
    if let Err(error) = outcome {
        return match backend.restore(previous) {
            Ok(()) => Err(error),
            Err(rollback) => Err(Error::Scheduler(format!(
                "{error}; restoring the previous registration also failed: {rollback}"
            ))),
        };
    }

    Ok(())
}

/// Only available to explicit, isolated native smoke tests.
#[cfg(feature = "native-smoke")]
pub fn isolated_native(id: &str) -> Result<Box<dyn Backend>> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(Error::Config(
            "invalid isolated scheduler identifier".into(),
        ));
    }

    #[cfg(windows)]
    {
        Ok(Box::new(windows::Windows::isolated(id)?))
    }

    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::Linux::isolated(id)?))
    }

    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::Macos::isolated(id)?))
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Err(Error::Scheduler("unsupported native scheduler".into()))
    }
}

/// Encoded only for Task Scheduler, which expands environment variables even without a shell.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerRequest {
    pub version: u32,
    pub config: PathBuf,
    pub directory: PathBuf,
    pub log: PathBuf,
}

impl RunnerRequest {
    pub fn from_spec(spec: &Spec) -> Self {
        Self {
            version: 1,
            config: spec.config.clone(),
            directory: spec.directory.clone(),
            log: spec.log.clone(),
        }
    }

    pub fn encode(&self) -> Result<String> {
        for path in [&self.config, &self.directory, &self.log] {
            render::checked_text(paths::utf8(path)?)?;
        }

        let bytes = serde_json::to_vec(self).map_err(|e| Error::Config(e.to_string()))?;
        if bytes.len() > 12000 {
            return Err(Error::Config(
                "scheduler paths exceed the Windows argument limit".into(),
            ));
        }

        const DIGITS: &[u8] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(bytes.len() * 2);

        for byte in bytes {
            encoded.push(DIGITS[(byte >> 4) as usize] as char);
            encoded.push(DIGITS[(byte & 15) as usize] as char);
        }

        Ok(encoded)
    }

    pub fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() > 24000
            || !encoded.len().is_multiple_of(2)
            || !encoded.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Config("invalid scheduled runner request".into()));
        }

        let bytes = (0..encoded.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&encoded[i..i + 2], 16).map_err(|e| Error::Config(e.to_string()))
            })
            .collect::<Result<Vec<_>>>()?;
        let request: Self =
            serde_json::from_slice(&bytes).map_err(|e| Error::Config(e.to_string()))?;
        if request.version != 1 {
            return Err(Error::Config("unsupported runner request version".into()));
        }

        Ok(request)
    }
}
