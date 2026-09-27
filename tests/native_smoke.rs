#![cfg(feature = "native-smoke")]
use fortify::{
    paths::AppPaths,
    schedule::{self, Backend, Spec, Timing},
};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::TempDir;

struct Cleanup(Box<dyn Backend>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Err(error) = self.0.disable() {
            eprintln!("Remove the isolated Fortify test job manually: {error}");
        }
    }
}

#[test]
#[ignore = "opt-in test creates and removes an isolated native scheduler registration"]
fn isolated_native_registration_round_trip() {
    assert_eq!(
        std::env::var("FORTIFY_NATIVE_SMOKE").as_deref(),
        Ok("1"),
        "set FORTIFY_NATIVE_SMOKE=1 to explicitly opt into native registration testing"
    );
    let id = format!(
        "{:x}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    );
    let temp = TempDir::new().unwrap();
    let input = temp.path().join("input");
    fs::create_dir(&input).unwrap();
    let app = AppPaths {
        config: temp.path().join("config.toml"),
        data: temp.path().join("state"),
    };

    fs::write(&app.config, "version=1").unwrap();
    let spec = Spec {
        version: 1,
        runner: PathBuf::from(env!("CARGO_BIN_EXE_fortify-runner")),
        config: app.config.clone(),
        directory: input,
        log: app.log(),
        timing: Timing::every("31d").unwrap(),
    };

    let mut backend = Cleanup(schedule::isolated_native(&id).unwrap());
    let initial = backend.0.snapshot().unwrap();

    assert!(
        initial.primary.is_none(),
        "isolated identifier unexpectedly already exists"
    );
    schedule::enable(&app, backend.0.as_mut(), &spec).unwrap();

    assert!(backend.0.snapshot().unwrap().enabled);
    schedule::disable(&app, backend.0.as_mut()).unwrap();
    let removed = backend.0.snapshot().unwrap();

    assert!(removed.primary.is_none() && !removed.enabled);
}
