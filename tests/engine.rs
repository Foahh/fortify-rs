use fortify::{
    Error, Result,
    config::{Config, Operations},
    engine, filesystem,
    journal::{self, DiskJournal, Journal, JournalWriter},
    plan::Action,
};
use std::{fs, path::Path, time::Duration};
use tempfile::TempDir;

fn config() -> Config {
    Config::parse(fortify::config::DEFAULT_CONFIG, None).unwrap()
}

fn options() -> Operations {
    Operations {
        sort: true,
        duplicates: false,
        prune: true,
    }
}

fn put(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn modified(path: &Path, seconds: i64) {
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(seconds, 0)).unwrap();
}

fn moves(plan: &fortify::plan::Plan) -> Vec<(&Path, &Path, &str)> {
    plan.actions
        .iter()
        .filter_map(|a| match a {
            Action::Move {
                from, to, reason, ..
            } => Some((from.as_path(), to.as_path(), reason.as_str())),
            _ => None,
        })
        .collect()
}

#[test]
fn preview_is_read_only_and_apply_undo_round_trips() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "報告 & 100%.PDF", "pdf");

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();

    assert_eq!(moves(&plan)[0].1, Path::new("Documents/報告 & 100%.PDF"));
    assert!(!temp.path().join(".fortify").exists());

    let report = engine::apply(temp.path(), &config(), options()).unwrap();

    assert_eq!(report.completed, 2);
    assert_eq!(
        fs::read_to_string(temp.path().join("Documents/報告 & 100%.PDF")).unwrap(),
        "pdf"
    );

    assert_eq!(engine::undo(temp.path()).unwrap(), 2);
    assert!(temp.path().join("報告 & 100%.PDF").exists());
    assert!(!temp.path().join("Documents").exists());
    assert!(!journal::last(temp.path()).exists());
    assert!(temp.path().join(".fortify/apply.lock").exists());
}

#[test]
fn opaque_folders_and_correct_nesting_are_preserved() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "Game/readme.pdf", "a");
    put(temp.path(), "Compressed/Game/code.rs", "b");
    put(temp.path(), "Pictures/Vacation/photo.png", "c");
    put(temp.path(), "Duplicates/old.pdf", "d");
    put(temp.path(), "waiting.tmp", "e");
    put(temp.path(), "Documents/already.pdf", "f");

    let plan = engine::preview(temp.path(), &config(), Operations::default()).unwrap();

    assert!(plan.actions.is_empty());
}

#[test]
fn nested_named_rules_win_before_extension_mapping() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "Screenshot.pdf", "a");
    put(temp.path(), "Pictures/Screenshots/already.png", "b");
    let cfg = Config::parse(
        "version=1\n\
         [mapping]\n\
         Documents=['pdf']\n\
         Pictures=['png']\n\
         [[rules]]\n\
         name='screenshots'\n\
         match=['Screenshot*']\n\
         folder='Pictures/Screenshots'\n",
        None,
    )
    .unwrap();

    let plan = engine::preview(temp.path(), &cfg, options()).unwrap();

    assert_eq!(
        moves(&plan),
        vec![(
            Path::new("Screenshot.pdf"),
            Path::new("Pictures/Screenshots/Screenshot.pdf"),
            "sort[screenshots]"
        )]
    );
}

#[test]
fn duplicates_keep_newest_and_have_one_final_move() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "old.txt", "same");
    put(temp.path(), "new.txt", "same");
    put(temp.path(), "different.txt", "diff");
    modified(&temp.path().join("old.txt"), 1000);
    modified(&temp.path().join("new.txt"), 2000);

    let plan = engine::preview(temp.path(), &config(), Operations::default()).unwrap();
    let old: Vec<_> = moves(&plan)
        .into_iter()
        .filter(|(from, _, _)| *from == Path::new("old.txt"))
        .collect();

    assert_eq!(
        old,
        vec![(
            Path::new("old.txt"),
            Path::new("Duplicates/old.txt"),
            "duplicate"
        )]
    );
    engine::apply_plan(plan, &mut DiskJournal).unwrap();

    assert!(temp.path().join("Documents/new.txt").exists());
    assert!(temp.path().join("Documents/different.txt").exists());

    engine::undo(temp.path()).unwrap();

    assert!(temp.path().join("old.txt").exists());
}

#[test]
fn equal_mtimes_break_ties_by_relative_path() {
    let temp = TempDir::new().unwrap();

    for file in ["z.txt", "a.txt", "b.txt"] {
        put(temp.path(), file, "same");
        modified(&temp.path().join(file), 1000);
    }

    let plan = engine::preview(temp.path(), &config(), Operations::default()).unwrap();
    let actions = moves(&plan);

    assert_eq!(actions[0].0, Path::new("a.txt"));
    assert_eq!(actions[0].2, "sort");
    assert_eq!(actions.iter().filter(|a| a.2 == "duplicate").count(), 2);
}

#[test]
fn collisions_preserve_case_and_existing_files() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "Report.PDF", "new");
    put(temp.path(), "Documents/Report.PDF", "old");
    fs::create_dir(temp.path().join("Documents/Report_1.PDF")).unwrap();

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();

    assert_eq!(moves(&plan)[0].1, Path::new("Documents/Report_2.PDF"));
    engine::apply_plan(plan, &mut DiskJournal).unwrap();

    assert_eq!(
        fs::read_to_string(temp.path().join("Documents/Report.PDF")).unwrap(),
        "old"
    );
}

#[test]
fn pruning_restores_original_directory_identity() {
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("Documents")).unwrap();
    let id = filesystem::identity(&temp.path().join("Documents")).unwrap();

    engine::apply(temp.path(), &config(), options()).unwrap();

    assert!(!temp.path().join("Documents").exists());

    engine::undo(temp.path()).unwrap();

    assert_eq!(
        id,
        filesystem::identity(&temp.path().join("Documents")).unwrap()
    );
}

#[test]
fn late_destination_never_overwrites_and_keeps_original() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "source");

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "Documents/report.pdf", "late arrival");

    assert!(engine::apply_plan(plan, &mut DiskJournal).is_err());
    assert_eq!(
        fs::read_to_string(temp.path().join("Documents/report.pdf")).unwrap(),
        "late arrival"
    );

    assert!(temp.path().join("report.pdf").exists());
}

#[test]
fn changed_source_is_refused_even_when_size_and_mtime_match() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "aaaa");
    modified(&temp.path().join("report.pdf"), 1000);

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "report.pdf", "bbbb");
    modified(&temp.path().join("report.pdf"), 1000);

    assert!(engine::apply_plan(plan, &mut DiskJournal).is_err());
    assert!(!temp.path().join("Documents").exists());
}

#[test]
fn partial_apply_stops_and_can_undo() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "first.pdf", "first");
    put(temp.path(), "second.pdf", "second");
    modified(&temp.path().join("first.pdf"), 2000);
    modified(&temp.path().join("second.pdf"), 1000);

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();
    fs::remove_file(temp.path().join("second.pdf")).unwrap();

    assert!(matches!(
        engine::apply_plan(plan, &mut DiskJournal),
        Err(Error::Partial { .. })
    ));

    assert!(temp.path().join("Documents/first.pdf").exists());
    assert!(engine::apply(temp.path(), &config(), options()).is_err());

    engine::undo(temp.path()).unwrap();

    assert!(temp.path().join("first.pdf").exists());
}

#[test]
fn undo_conflict_is_retryable_and_never_overwrites() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "original");

    engine::apply(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "report.pdf", "conflict");

    assert!(engine::undo(temp.path()).is_err());
    assert_eq!(
        fs::read_to_string(temp.path().join("report.pdf")).unwrap(),
        "conflict"
    );
    fs::remove_file(temp.path().join("report.pdf")).unwrap();

    engine::undo(temp.path()).unwrap();

    assert_eq!(
        fs::read_to_string(temp.path().join("report.pdf")).unwrap(),
        "original"
    );
}

#[test]
fn undo_refuses_changed_contents() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "original");

    engine::apply(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "Documents/report.pdf", "edited");

    assert!(engine::undo(temp.path()).is_err());
    assert!(!temp.path().join("report.pdf").exists());
}

struct FailWrite {
    at: usize,
    count: usize,
}

impl JournalWriter for FailWrite {
    fn write(&mut self, path: &Path, journal: &Journal) -> Result<()> {
        self.count += 1;
        if self.count == self.at {
            return Err(Error::Operation("injected journal write failure".into()));
        }

        DiskJournal.write(path, journal)
    }
}

#[test]
fn interrupted_move_recovers_from_durable_intent() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "data");

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();
    let mut writer = FailWrite { at: 5, count: 0 };

    assert!(engine::apply_plan(plan, &mut writer).is_err());
    assert!(temp.path().join("Documents/report.pdf").exists());

    engine::undo(temp.path()).unwrap();

    assert!(temp.path().join("report.pdf").exists());
}

#[test]
fn undo_recovers_if_completion_write_was_interrupted() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "data");

    engine::apply(temp.path(), &config(), options()).unwrap();
    let mut writer = FailWrite { at: 3, count: 0 };

    assert!(engine::undo_with_writer(temp.path(), &mut writer).is_err());
    assert!(temp.path().join("report.pdf").exists());

    engine::undo(temp.path()).unwrap();

    assert!(!temp.path().join("Documents").exists());
}

#[test]
fn initial_journal_failure_performs_no_target_mutation() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "data");

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();

    assert!(engine::apply_plan(plan, &mut FailWrite { at: 2, count: 0 }).is_err());
    assert!(temp.path().join("report.pdf").exists());
    assert!(!temp.path().join("Documents").exists());
    assert!(!journal::pending(temp.path()).exists());
}

#[test]
fn noop_and_zero_mutation_failure_preserve_previous_undo() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "data");

    engine::apply(temp.path(), &config(), options()).unwrap();
    let previous = fs::read(journal::last(temp.path())).unwrap();

    engine::apply(temp.path(), &config(), options()).unwrap();

    assert_eq!(previous, fs::read(journal::last(temp.path())).unwrap());
    put(temp.path(), "next.pdf", "next");

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "next.pdf", "changed");

    assert!(engine::apply_plan(plan, &mut DiskJournal).is_err());
    assert_eq!(previous, fs::read(journal::last(temp.path())).unwrap());
}

#[test]
fn invalid_journals_are_not_overwritten() {
    for content in ["{}", r#"{"version":1,"records":[]}"#, "{"] {
        let temp = TempDir::new().unwrap();
        put(temp.path(), ".fortify/last-run.json", content);
        put(temp.path(), "file.txt", "data");

        let error = engine::apply(temp.path(), &config(), options()).unwrap_err();

        assert!(error.to_string().contains("invalid journal"));
        assert!(error.to_string().contains("last-run.json"));
        assert_eq!(
            fs::read_to_string(journal::last(temp.path())).unwrap(),
            content
        );
        assert!(temp.path().join("file.txt").exists());
        assert!(!temp.path().join("Documents").exists());
    }
}

#[test]
fn native_rename_does_not_replace() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "from", "a");
    put(temp.path(), "to", "b");

    assert!(
        filesystem::rename_no_replace(&temp.path().join("from"), &temp.path().join("to")).is_err()
    );

    assert_eq!(fs::read_to_string(temp.path().join("to")).unwrap(), "b");
}

#[test]
fn held_lock_prevents_apply_and_releases_on_drop() {
    let temp = TempDir::new().unwrap();
    let root = filesystem::target(temp.path()).unwrap();
    let guard = filesystem::TargetLock::acquire(&root).unwrap();

    assert!(matches!(
        engine::apply(&root, &config(), options()),
        Err(Error::Locked(_))
    ));
    drop(guard);

    engine::apply(&root, &config(), options()).unwrap();
}

#[test]
fn lock_released_when_child_process_is_killed() {
    let temp = TempDir::new().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "lock_child"])
        .env("FORTIFY_TEST_LOCK_ROOT", temp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut ready = false;

    for _ in 0..200 {
        if temp.path().join("ready").exists() {
            ready = true;
            break;
        }

        std::thread::sleep(Duration::from_millis(25));
    }

    if !ready {
        let _ = child.kill();
        let _ = child.wait();
        panic!("child did not acquire lock");
    }

    let root = filesystem::target(temp.path()).unwrap();

    assert!(matches!(
        filesystem::TargetLock::acquire(&root),
        Err(Error::Locked(_))
    ));
    child.kill().unwrap();
    child.wait().unwrap();
    let _guard = filesystem::TargetLock::acquire(&root).unwrap();
}

#[test]
#[ignore = "child process helper"]
fn lock_child() {
    let Some(root) = std::env::var_os("FORTIFY_TEST_LOCK_ROOT") else {
        return;
    };

    let root = filesystem::target(Path::new(&root)).unwrap();
    let _guard = filesystem::TargetLock::acquire(&root).unwrap();
    fs::write(root.join("ready"), "").unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[test]
fn links_are_opaque() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    put(outside.path(), "outside.pdf", "leave alone");
    let link = temp.path().join("Documents");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    #[cfg(windows)]
    {
        // Junction creation works without Developer Mode or symlink privileges.

        let output = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();

    assert!(plan.actions.is_empty());
    assert_eq!(
        fs::read_to_string(outside.path().join("outside.pdf")).unwrap(),
        "leave alone"
    );

    // Remove only the link, never recursively traverse the junction.
    #[cfg(windows)]
    fs::remove_dir(link).unwrap();
    #[cfg(unix)]
    fs::remove_file(link).unwrap();
}

#[test]
fn journal_paths_cannot_escape_target() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "report.pdf", "data");

    engine::apply(temp.path(), &config(), options()).unwrap();
    let path = journal::last(temp.path());
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["records"][1]["operation"]["from"] = "../outside".into();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

    assert!(
        engine::undo(temp.path())
            .unwrap_err()
            .to_string()
            .contains("invalid relative path")
    );
}

#[test]
fn journal_from_another_target_is_rejected() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    put(a.path(), "file.pdf", "data");

    engine::apply(a.path(), &config(), options()).unwrap();
    fs::create_dir(b.path().join(".fortify")).unwrap();
    fs::copy(journal::last(a.path()), journal::last(b.path())).unwrap();

    assert!(engine::undo(b.path()).is_err());
}

#[test]
fn config_relative_target_is_resolved_against_config_location() {
    let temp = TempDir::new().unwrap();
    let cfg = Config::parse(
        "version=1\ndirectory='Downloads'\n",
        Some(temp.path().join("config.toml")),
    )
    .unwrap();

    assert_eq!(cfg.target(None).unwrap(), temp.path().join("Downloads"));
}

#[test]
fn every_apply_journal_boundary_remains_recoverable() {
    for at in 1..=7 {
        let temp = TempDir::new().unwrap();
        put(temp.path(), "report.pdf", "data");

        let plan = engine::preview(temp.path(), &config(), options()).unwrap();

        let result = engine::apply_plan(plan, &mut FailWrite { at, count: 0 });

        assert!(result.is_err(), "failure {at} was not injected");
        if journal::pending(temp.path()).exists() || journal::last(temp.path()).exists() {
            engine::undo(temp.path())
                .unwrap_or_else(|e| panic!("failure {at} did not recover: {e}"));
        }

        assert!(
            temp.path().join("report.pdf").exists(),
            "failure {at} lost source"
        );

        assert!(
            !temp.path().join("Documents").exists(),
            "failure {at} left directory"
        );
    }
}

#[test]
fn every_undo_journal_boundary_is_retryable() {
    for at in 1..=5 {
        let temp = TempDir::new().unwrap();
        put(temp.path(), "report.pdf", "data");

        engine::apply(temp.path(), &config(), options()).unwrap();

        assert!(engine::undo_with_writer(temp.path(), &mut FailWrite { at, count: 0 }).is_err());

        engine::undo(temp.path()).unwrap_or_else(|e| panic!("failure {at} did not recover: {e}"));

        assert!(temp.path().join("report.pdf").exists());
        assert!(!temp.path().join("Documents").exists());
    }
}

#[test]
fn ignored_files_prevent_pruning() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "Documents/desktop.ini", "ignored");

    assert!(
        engine::preview(temp.path(), &config(), options())
            .unwrap()
            .actions
            .is_empty()
    );
}

#[test]
fn saved_config_inside_target_is_never_sorted() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("config.toml");
    fs::write(&source, "version=1").unwrap();
    let config = Config::parse("version=1", Some(source.clone())).unwrap();

    assert!(
        engine::preview(temp.path(), &config, Operations::default())
            .unwrap()
            .actions
            .is_empty()
    );

    assert!(source.exists());
}

#[test]
fn reoccupied_empty_directory_stops_pruning() {
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("Documents")).unwrap();

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "Documents/late.pdf", "data");

    assert!(engine::apply_plan(plan, &mut DiskJournal).is_err());
    assert!(temp.path().join("Documents/late.pdf").exists());
}

#[test]
fn second_run_replaces_only_last_run_history() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "first.pdf", "first");

    engine::apply(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "second.pdf", "second");

    engine::apply(temp.path(), &config(), options()).unwrap();

    engine::undo(temp.path()).unwrap();

    assert!(temp.path().join("Documents/first.pdf").exists());
    assert!(temp.path().join("second.pdf").exists());
    assert!(engine::undo(temp.path()).is_err());
}

#[test]
fn recovery_after_a_partial_second_run_does_not_restore_old_history() {
    let temp = TempDir::new().unwrap();
    put(temp.path(), "first.pdf", "first");

    engine::apply(temp.path(), &config(), options()).unwrap();
    put(temp.path(), "second.pdf", "second");

    let plan = engine::preview(temp.path(), &config(), options()).unwrap();

    // Existing Documents directory: intent for the move is write 2, completion is write 3.
    assert!(engine::apply_plan(plan, &mut FailWrite { at: 3, count: 0 }).is_err());

    engine::undo(temp.path()).unwrap();

    assert!(temp.path().join("second.pdf").exists());
    assert!(temp.path().join("Documents/first.pdf").exists());
    assert!(!journal::last(temp.path()).exists());
}

#[cfg(unix)]
#[test]
fn non_unicode_names_are_refused_before_mutation() {
    use std::os::unix::ffi::OsStringExt;

    let temp = TempDir::new().unwrap();
    let path = temp.path().join(std::ffi::OsString::from_vec(vec![
        0xff, b'.', b'p', b'd', b'f',
    ]));
    fs::write(&path, "data").unwrap();

    assert!(engine::preview(temp.path(), &config(), options()).is_err());
    assert!(path.exists());
    assert!(!temp.path().join(".fortify").exists());
}
