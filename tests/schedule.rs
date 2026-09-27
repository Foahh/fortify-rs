use fortify::{
    Error, Result,
    paths::AppPaths,
    schedule::{self, Backend, Registration, Spec, Timing, render},
};
use std::{fs, path::Path};
use tempfile::TempDir;

fn setup(root: &Path) -> (AppPaths, Spec) {
    let app = AppPaths {
        config: root.join("config.toml"),
        data: root.join("state"),
    };

    fs::write(&app.config, "version=1").unwrap();
    let runner = root.join("runner.exe");
    fs::write(&runner, "").unwrap();
    let spec = Spec {
        version: 1,
        runner,
        config: app.config.clone(),
        directory: root.to_owned(),
        log: app.log(),
        timing: Timing::every("1h").unwrap(),
    };

    (app, spec)
}

#[derive(Default)]
struct Fake {
    current: Registration,
    fail_install: bool,
    fail_restore: bool,
    installs: usize,
    restores: usize,
}

impl Backend for Fake {
    fn snapshot(&mut self) -> Result<Registration> {
        Ok(self.current.clone())
    }

    fn install(&mut self, _: &Spec) -> Result<()> {
        self.installs += 1;
        self.current = Registration {
            primary: Some("new".into()),
            secondary: None,
            enabled: true,
            autostart: true,
        };

        if self.fail_install {
            Err(Error::Scheduler("injected registration failure".into()))
        } else {
            Ok(())
        }
    }

    fn restore(&mut self, previous: &Registration) -> Result<()> {
        self.restores += 1;
        if self.fail_restore {
            return Err(Error::Scheduler("injected rollback failure".into()));
        }

        self.current = previous.clone();

        Ok(())
    }

    fn disable(&mut self) -> Result<()> {
        self.current = Registration::default();

        Ok(())
    }
}

#[test]
fn timing_accepts_boundaries_and_rejects_invalid_values() {
    for value in ["1m", "30m", "1h", "6h", "1d", "31d", "744h"] {
        Timing::every(value).unwrap();
    }

    for value in [
        "",
        "0m",
        "32d",
        "999999999999999999999d",
        "1s",
        "1.5h",
        "+1h",
        "1 h",
        "daily",
    ] {
        assert!(Timing::every(value).is_err(), "accepted {value}");
    }

    for value in ["00:00", "09:00", "23:59"] {
        Timing::daily(value).unwrap();
    }

    for value in ["9:00", "24:00", "23:60", "no:no", "+1:00"] {
        assert!(Timing::daily(value).is_err());
    }
}

#[test]
fn registration_failure_restores_previous_schedule_and_state() {
    let temp = TempDir::new().unwrap();
    let (app, spec) = setup(temp.path());
    let mut backend = Fake::default();
    schedule::enable(&app, &mut backend, &spec).unwrap();
    let before = backend.current.clone();
    let state = fs::read(app.schedule()).unwrap();
    backend.fail_install = true;

    assert!(schedule::enable(&app, &mut backend, &spec).is_err());
    assert_eq!(backend.current, before);
    assert_eq!(fs::read(app.schedule()).unwrap(), state);
    assert_eq!(backend.restores, 1);
}

#[test]
fn failed_state_write_rolls_back_native_registration() {
    let temp = TempDir::new().unwrap();
    let (app, spec) = setup(temp.path());
    fs::create_dir_all(app.schedule()).unwrap();

    // An absent old state allows registration, but a directory cannot be a state file.
    // Instead inject the collision during install to exercise post-registration rollback.
    fs::remove_dir(app.schedule()).unwrap();
    struct Collision<'a> {
        path: &'a Path,
        fake: Fake,
    }

    impl Backend for Collision<'_> {
        fn snapshot(&mut self) -> Result<Registration> {
            self.fake.snapshot()
        }

        fn install(&mut self, spec: &Spec) -> Result<()> {
            self.fake.install(spec)?;
            fs::create_dir(self.path).unwrap();

            Ok(())
        }

        fn restore(&mut self, old: &Registration) -> Result<()> {
            self.fake.restore(old)
        }

        fn disable(&mut self) -> Result<()> {
            self.fake.disable()
        }
    }

    let path = app.schedule();
    let mut backend = Collision {
        path: &path,
        fake: Fake::default(),
    };

    assert!(schedule::enable(&app, &mut backend, &spec).is_err());
    assert_eq!(backend.fake.current, Registration::default());
    assert_eq!(backend.fake.restores, 1);
}

#[test]
fn rollback_failure_reports_both_errors() {
    let temp = TempDir::new().unwrap();
    let (app, spec) = setup(temp.path());
    let mut backend = Fake {
        fail_install: true,
        fail_restore: true,
        ..Default::default()
    };

    let error = schedule::enable(&app, &mut backend, &spec)
        .unwrap_err()
        .to_string();

    assert!(error.contains("registration failure"));
    assert!(error.contains("rollback failure"));
}

#[test]
fn disable_keeps_configuration_and_removes_only_schedule_state() {
    let temp = TempDir::new().unwrap();
    let (app, spec) = setup(temp.path());
    let mut backend = Fake::default();
    schedule::enable(&app, &mut backend, &spec).unwrap();
    schedule::disable(&app, &mut backend).unwrap();

    assert!(app.config.exists());
    assert!(!app.schedule().exists());
    assert_eq!(backend.current, Registration::default());
}

#[test]
fn renderer_escapes_native_formats_without_a_shell() {
    let temp = TempDir::new().unwrap();
    let (_, mut spec) = setup(temp.path());
    spec.config = "/tmp/a %n $HOME & \"quote\"/config.toml".into();
    let (service, timer) = render::systemd(&spec).unwrap();

    assert!(service.contains("%%n"));
    assert!(service.contains("$$HOME"));
    assert!(service.contains("\\\"quote\\\""));
    assert!(!service.contains("sh -c"));
    assert!(timer.contains("OnUnitActiveSec=3600s"));

    let plist = render::launchd(&spec, "com.fortify.test").unwrap();

    assert!(plist.contains("&amp;"));
    assert!(plist.contains("&quot;quote&quot;"));
    assert!(plist.contains("<key>StartInterval</key><integer>3600</integer>"));

    let xml = render::windows_xml(&spec, "domain\\user", "2026-01-01T09:00:00").unwrap();

    assert!(xml.contains("InteractiveToken"));
    assert!(xml.contains("<Interval>PT3600S</Interval>"));
    assert!(!xml.contains("$HOME"));
    assert!(xml.contains("--request "));
    assert_eq!(render::windows_arg("C:\\dir\\").unwrap(), "\"C:\\dir\\\\\"");
    assert_eq!(render::windows_arg("a\"b").unwrap(), "\"a\\\"b\"");
}

#[test]
fn daily_renderer_uses_wall_clock_time() {
    let temp = TempDir::new().unwrap();
    let (_, mut spec) = setup(temp.path());
    spec.timing = Timing::daily("09:07").unwrap();

    assert!(
        render::systemd(&spec)
            .unwrap()
            .1
            .contains("OnCalendar=*-*-* 09:07:00")
    );

    assert!(
        render::launchd(&spec, "com.fortify.test")
            .unwrap()
            .contains("<key>Hour</key><integer>9</integer>")
    );

    assert!(
        render::windows_xml(&spec, "user", "2026-01-01T09:07:00")
            .unwrap()
            .contains("<CalendarTrigger>")
    );
}

#[test]
fn scheduler_paths_reject_control_character_injection() {
    assert!(render::systemd_arg("path\nExecStart=bad").is_err());
    assert!(render::windows_arg("path\rnext").is_err());
    assert!(render::xml("path\0").is_err());
}

#[test]
fn windows_executable_percent_paths_are_rejected_before_registration() {
    let temp = TempDir::new().unwrap();
    let (_, mut spec) = setup(temp.path());
    spec.runner = r"C:\%TEMP%\fortify-runner.exe".into();

    assert!(render::windows_xml(&spec, "user", "2026-01-01T09:00:00").is_err());
}

#[test]
fn encoded_runner_requests_reject_malformed_payloads() {
    use fortify::schedule::RunnerRequest;

    for text in ["", "0", "gg", "00", &"a".repeat(24002)] {
        assert!(RunnerRequest::decode(text).is_err());
    }
}
