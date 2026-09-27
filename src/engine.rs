use crate::{
    Error, Result,
    config::{Config, Operations},
    error::IoContext,
    filesystem::{self, TargetLock},
    journal::{self, DiskJournal, Journal, JournalWriter, Operation, Record, State},
    plan::{self, Action, Plan},
};
use std::{fs, path::Path};

#[derive(Debug)]
pub struct ApplyReport {
    pub plan: Plan,
    pub completed: usize,
}

pub fn preview(target: &Path, config: &Config, options: Operations) -> Result<Plan> {
    let root = filesystem::target(target)?;
    plan::build(&root, config, options)
}

pub fn apply(target: &Path, config: &Config, options: Operations) -> Result<ApplyReport> {
    let root = filesystem::target(target)?;
    let _lock = TargetLock::acquire(&root)?;
    check_ready(&root)?;

    let plan = plan::build(&root, config, options)?;
    execute(plan, &mut DiskJournal)
}

// Library entry point for a previously built plan. Every action is revalidated.
pub fn apply_plan(plan: Plan, writer: &mut dyn JournalWriter) -> Result<ApplyReport> {
    let root = filesystem::target(&plan.target)?;
    if root != plan.target {
        return Err(Error::Operation("plan target must be canonical".into()));
    }

    let _lock = TargetLock::acquire(&root)?;
    check_ready(&root)?;

    execute(plan, writer)
}

fn check_ready(root: &Path) -> Result<()> {
    if journal::read(&journal::pending(root), root)?.is_some() {
        return Err(Error::Recovery(
            "an interrupted run exists; run fortify undo before applying again".into(),
        ));
    }

    if let Some(previous) = journal::read(&journal::last(root), root)?
        && (!previous.complete || previous.records.iter().any(|r| r.state != State::Done))
    {
        return Err(Error::Recovery(
            "an interrupted undo exists; run fortify undo again".into(),
        ));
    }

    Ok(())
}

fn execute(plan: Plan, writer: &mut dyn JournalWriter) -> Result<ApplyReport> {
    if plan.actions.is_empty() {
        return Ok(ApplyReport { plan, completed: 0 });
    }

    let root = plan.target.clone();
    let old = journal::read(&journal::last(&root), &root)?;

    let temporary = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(filesystem::internal_dir(&root)?)
        .context("create journal staging")?;
    let staging = temporary
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let _kept = temporary.keep();

    let mut journal = Journal {
        version: 1,
        root: root.clone(),
        root_identity: filesystem::identity(&root)?,
        staging,
        complete: false,
        records: vec![],
    };

    let result = (|| {
        writer.write(&journal::pending(&root), &journal)?;

        for action in &plan.actions {
            match action {
                Action::Move {
                    from,
                    to,
                    stamp,
                    digest,
                    ..
                } => {
                    let source = filesystem::checked_path(&root, from)?;
                    verify_file(&source, stamp, digest)?;

                    let destination = filesystem::checked_path(&root, to)?;
                    if filesystem::metadata(&destination)?.is_some() {
                        return Err(Error::Operation(format!(
                            "destination appeared: {}",
                            destination.display()
                        )));
                    }

                    ensure_parents(&mut journal, to.parent().unwrap_or(Path::new("")), writer)?;
                    record_and_apply(
                        &mut journal,
                        Operation::Move {
                            from: from.clone(),
                            to: to.clone(),
                            stamp: stamp.clone(),
                            digest: digest.clone(),
                        },
                        writer,
                    )?;
                }

                Action::RemoveDir {
                    path,
                    identity: expected,
                } => {
                    let directory = filesystem::checked_path(&root, path)?;
                    ensure_empty(&directory)?;
                    let identity = filesystem::identity(&directory)?;
                    if identity != *expected {
                        return Err(Error::Operation(
                            "pruning candidate changed since planning".into(),
                        ));
                    }

                    let slot = format!("dir-{}", journal.records.len());
                    record_and_apply(
                        &mut journal,
                        Operation::Directory {
                            path: path.clone(),
                            slot,
                            identity,
                            creating: false,
                        },
                        writer,
                    )?;
                }
            }
        }

        journal.complete = true;
        writer.write(&journal::pending(&root), &journal)?;
        writer.write(&journal::last(&root), &journal)?;
        journal::remove_file(&journal::pending(&root))?;

        Ok(())
    })();

    match result {
        Ok(()) => {
            // New history is committed before retiring the previous empty-directory backups.
            if let Some(old) = old {
                journal::cleanup(&old)?;
            }

            Ok(ApplyReport {
                completed: journal.records.len(),
                plan,
            })
        }

        Err(error) => {
            let mut changed = false;
            let mut ambiguous = false;

            for record in &journal.records {
                match position(&journal, &record.operation) {
                    Ok(Position::After) => changed = true,
                    Ok(Position::Before) => {}
                    Err(_) => ambiguous = true,
                }
            }

            if !changed && !ambiguous {
                journal::remove_file(&journal::pending(&root))?;
                journal::cleanup(&journal)?;
                return Err(error);
            }

            Err(Error::Partial {
                message: error.to_string(),
                completed: journal
                    .records
                    .iter()
                    .filter(|r| r.state == State::Done)
                    .count(),
                target: root,
            })
        }
    }
}

fn ensure_parents(
    journal: &mut Journal,
    parent: &Path,
    writer: &mut dyn JournalWriter,
) -> Result<()> {
    let mut relative = std::path::PathBuf::new();

    for component in parent.components() {
        relative.push(component);
        let path = filesystem::checked_path(&journal.root, &relative)?;
        if let Some(meta) = filesystem::metadata(&path)? {
            if !meta.is_dir() {
                return Err(Error::Operation(format!(
                    "parent is not a directory: {}",
                    path.display()
                )));
            }
        } else {
            let slot = format!("dir-{}", journal.records.len());
            let staging = journal::staging_path(journal)?.join(&slot);
            fs::create_dir(&staging).context("prepare directory")?;
            let identity = filesystem::identity(&staging)?;
            record_and_apply(
                journal,
                Operation::Directory {
                    path: relative.clone(),
                    slot,
                    identity,
                    creating: true,
                },
                writer,
            )?;
        }
    }

    Ok(())
}

fn record_and_apply(
    journal: &mut Journal,
    operation: Operation,
    writer: &mut dyn JournalWriter,
) -> Result<()> {
    journal.records.push(Record {
        operation: operation.clone(),
        state: State::Prepared,
    });
    writer.write(&journal::pending(&journal.root), journal)?;

    perform(journal, &operation, false)?;

    journal.records.last_mut().unwrap().state = State::Done;
    writer.write(&journal::pending(&journal.root), journal)
}

fn verify_file(path: &Path, stamp: &filesystem::Stamp, digest: &str) -> Result<()> {
    filesystem::verify_stamp(path, stamp)?;
    if filesystem::hash(path)? != digest {
        return Err(Error::Operation(format!(
            "file contents changed: {}",
            path.display()
        )));
    }

    filesystem::verify_stamp(path, stamp)
}

fn ensure_empty(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).context("inspect empty directory")?;
    if filesystem::is_link(&meta) || !meta.is_dir() {
        return Err(Error::Operation(format!(
            "not a real directory: {}",
            path.display()
        )));
    }

    if fs::read_dir(path)
        .context("read empty directory")?
        .next()
        .is_some()
    {
        return Err(Error::Operation(format!(
            "directory is no longer empty: {}",
            path.display()
        )));
    }

    Ok(())
}

#[derive(PartialEq)]
enum Position {
    Before,
    After,
}

fn position(journal: &Journal, operation: &Operation) -> Result<Position> {
    let (from, to) = endpoints(journal, operation)?;
    let before = filesystem::metadata(&from)?.is_some();
    let after = filesystem::metadata(&to)?.is_some();
    let (path, position) = match (before, after) {
        (true, false) => (&from, Position::Before),
        (false, true) => (&to, Position::After),
        _ => {
            return Err(Error::Recovery(format!(
                "ambiguous operation: {} and {}; restore the expected file/directory \
                 or move conflicting entries aside, then retry undo",
                from.display(),
                to.display()
            )));
        }
    };

    match operation {
        Operation::Move { stamp, digest, .. } => verify_file(path, stamp, digest)?,
        Operation::Directory { identity, .. } => {
            let meta = fs::symlink_metadata(path).context("inspect recorded directory")?;
            if filesystem::is_link(&meta)
                || !meta.is_dir()
                || filesystem::identity(path)? != *identity
            {
                return Err(Error::Recovery(format!(
                    "directory identity changed: {}",
                    path.display()
                )));
            }
        }
    }

    Ok(position)
}

fn endpoints(
    journal: &Journal,
    operation: &Operation,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    match operation {
        Operation::Move { from, to, .. } => Ok((
            filesystem::checked_path(&journal.root, from)?,
            filesystem::checked_path(&journal.root, to)?,
        )),
        Operation::Directory {
            path,
            slot,
            creating,
            ..
        } => {
            let path = filesystem::checked_path(&journal.root, path)?;
            let stash = journal::staging_path(journal)?.join(slot);
            if *creating {
                Ok((stash, path))
            } else {
                Ok((path, stash))
            }
        }
    }
}

fn perform(journal: &Journal, operation: &Operation, undo: bool) -> Result<()> {
    let expected = if undo {
        Position::After
    } else {
        Position::Before
    };

    if position(journal, operation)? != expected {
        return Err(Error::Recovery(
            "operation has already moved; retry undo to reconcile".into(),
        ));
    }

    let (from, to) = endpoints(journal, operation)?;
    let (from, to) = if undo { (to, from) } else { (from, to) };
    if matches!(operation, Operation::Directory { .. }) {
        ensure_empty(&from)?;
    }

    filesystem::rename_no_replace(&from, &to)
}

pub fn undo(target: &Path) -> Result<usize> {
    undo_with_writer(target, &mut DiskJournal)
}

pub fn undo_with_writer(target: &Path, writer: &mut dyn JournalWriter) -> Result<usize> {
    let root = filesystem::target(target)?;
    let _lock = TargetLock::acquire(&root)?;
    let pending_path = journal::pending(&root);
    let (path, mut journal, is_pending) = if let Some(j) = journal::read(&pending_path, &root)? {
        (pending_path, j, true)
    } else if let Some(j) = journal::read(&journal::last(&root), &root)? {
        (journal::last(&root), j, false)
    } else {
        return Err(Error::Recovery("no journal found to undo".into()));
    };

    let mut reversed = 0;
    let mut had_changes = journal.records.iter().any(|r| r.state != State::Prepared);
    journal.complete = false;
    writer.write(&path, &journal)?;

    // Reverse order is essential: child moves must be restored before their parent directory.
    for i in (0..journal.records.len()).rev() {
        let record = journal.records[i].clone();
        if record.state == State::Undone {
            continue;
        }

        let actual = position(&journal, &record.operation)?;

        match record.state {
            State::Prepared if actual == Position::Before => {
                journal.records[i].state = State::Undone;
                writer.write(&path, &journal)?;
                continue;
            }

            State::Undoing if actual == Position::Before => {
                journal.records[i].state = State::Undone;
                writer.write(&path, &journal)?;
                had_changes = true;
                continue;
            }

            _ if actual == Position::Before => {
                return Err(Error::Recovery(
                    "a recorded completed operation was changed outside Fortify".into(),
                ));
            }

            _ => {}
        }

        had_changes = true;
        journal.records[i].state = State::Undoing;
        writer.write(&path, &journal)?;

        perform(&journal, &record.operation, true)?;

        journal.records[i].state = State::Undone;
        writer.write(&path, &journal)?;
        reversed += 1;
    }

    // Marked Undone records remain recoverable even if cleanup is interrupted.
    journal::cleanup(&journal)?;
    if is_pending && had_changes {
        if let Some(previous) = journal::read(&journal::last(&root), &root)?
            && previous.staging != journal.staging
        {
            journal::cleanup(&previous)?;
        }

        journal::remove_file(&journal::last(&root))?;
    }

    journal::remove_file(&path)?;

    Ok(reversed)
}
