use super::{Backend, Registration, Spec, Timing, render};
use crate::{Error, Result};
use windows::{
    Win32::System::{
        Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        TaskScheduler::{
            ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN,
            TaskScheduler,
        },
        Variant::VARIANT,
    },
    core::BSTR,
};

fn win<T>(value: windows::core::Result<T>) -> Result<T> {
    value.map_err(|e| Error::Scheduler(format!("Windows Task Scheduler: {e}")))
}

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: this guard exists only after successful initialization on this thread.
        unsafe { CoUninitialize() };
    }
}

pub struct Windows {
    folder: ITaskFolder,
    name: BSTR,
    user: String,
    _service: ITaskService,
    _apartment: Apartment,
}

impl Windows {
    pub fn new() -> Result<Self> {
        Self::named(None)
    }

    #[cfg(feature = "native-smoke")]
    pub fn isolated(id: &str) -> Result<Self> {
        Self::named(Some(id))
    }

    fn named(id: Option<&str>) -> Result<Self> {
        // SAFETY: COM interfaces remain on this thread and are released before the apartment.
        unsafe {
            win(CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok())?;
            let apartment = Apartment;
            let service: ITaskService =
                win(CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER))?;
            let empty = VARIANT::default();
            win(service.Connect(&empty, &empty, &empty, &empty))?;
            let user = win(service.ConnectedUser())?.to_string();
            let domain = win(service.ConnectedDomain())?.to_string();
            let user = if domain.is_empty() {
                user
            } else {
                format!("{domain}\\{user}")
            };

            let suffix = blake3::hash(user.as_bytes()).to_hex()[..12].to_owned();
            let folder = win(service.GetFolder(&BSTR::from("\\")))?;

            Ok(Self {
                folder,
                name: BSTR::from(id.map_or_else(
                    || format!("Fortify-{suffix}"),
                    |id| format!("Fortify-Test-{id}"),
                )),
                user,
                _service: service,
                _apartment: apartment,
            })
        }
    }

    fn register(&self, xml: &str, enabled: bool) -> Result<()> {
        // SAFETY: the live COM folder belongs to the current initialized apartment.
        unsafe {
            let empty = VARIANT::default();
            let user = VARIANT::from(self.user.as_str());
            let task = win(self.folder.RegisterTask(
                &self.name,
                &BSTR::from(xml),
                TASK_CREATE_OR_UPDATE.0,
                &user,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            ))?;
            win(task.SetEnabled(enabled.into()))
        }
    }
}

impl Backend for Windows {
    fn snapshot(&mut self) -> Result<Registration> {
        // SAFETY: COM arguments are owned and live throughout each synchronous call.
        unsafe {
            match self.folder.GetTask(&self.name) {
                Ok(task) => Ok(Registration {
                    primary: Some(win(task.Xml())?.to_string()),
                    secondary: None,
                    enabled: win(task.Enabled())?.as_bool(),
                    autostart: win(task.Enabled())?.as_bool(),
                }),
                Err(e) if e.code().0 as u32 == 0x80070002 => Ok(Registration::default()),
                Err(e) => Err(Error::Scheduler(format!("query task: {e}"))),
            }
        }
    }

    fn install(&mut self, spec: &Spec) -> Result<()> {
        let now = time::OffsetDateTime::now_local()
            .map_err(|e| Error::Scheduler(format!("read local time: {e}")))?;
        let start = match spec.timing {
            Timing::Every { seconds } => now + time::Duration::seconds(seconds as i64),
            Timing::Daily { hour, minute } => {
                let start = now.replace_time(
                    time::Time::from_hms(hour, minute, 0)
                        .map_err(|e| Error::Config(e.to_string()))?,
                );
                if start <= now {
                    start + time::Duration::days(1)
                } else {
                    start
                }
            }
        };

        let boundary = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            start.year(),
            start.month() as u8,
            start.day(),
            start.hour(),
            start.minute(),
            start.second()
        );
        self.register(&render::windows_xml(spec, &self.user, &boundary)?, true)
    }

    fn restore(&mut self, previous: &Registration) -> Result<()> {
        match &previous.primary {
            Some(xml) => self.register(xml, previous.enabled),
            None => self.disable(),
        }
    }

    fn disable(&mut self) -> Result<()> {
        if self.snapshot()?.primary.is_some() {
            // SAFETY: folder and task name are valid owned COM values.
            win(unsafe { self.folder.DeleteTask(&self.name, 0) })?;
        }

        Ok(())
    }
}
