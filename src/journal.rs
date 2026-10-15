use crate::{
    Error, Result,
    error::IoContext,
    filesystem::{self, Identity, Stamp},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Prepared,
    Done,
    Undoing,
    Undone,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Move {
        from: PathBuf,
        to: PathBuf,
        stamp: Stamp,
        digest: String,
    },
    Directory {
        path: PathBuf,
        slot: String,
        identity: Identity,
        creating: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub operation: Operation,
    pub state: State,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub root: PathBuf,
    pub root_identity: Identity,
    pub staging: String,
    pub complete: bool,
    pub records: Vec<Record>,
}

pub trait JournalWriter {
    fn write(&mut self, path: &Path, journal: &Journal) -> Result<()>;

    // Snapshot-only writers remain usable for injection and alternate storage.
    fn write_record(&mut self, path: &Path, journal: &Journal, _index: usize) -> Result<()> {
        self.write(path, journal)
    }
}

pub struct DiskJournal;

impl JournalWriter for DiskJournal {
    fn write(&mut self, path: &Path, journal: &Journal) -> Result<()> {
        let mut data = serde_json::to_vec(journal).map_err(|e| Error::Recovery(e.to_string()))?;
        data.push(b'\n');
        filesystem::atomic_write(path, &data)
    }

    fn write_record(&mut self, path: &Path, journal: &Journal, index: usize) -> Result<()> {
        if journal.version != 2 {
            return self.write(path, journal);
        }

        let update = Update {
            staging: journal.staging.clone(),
            index,
            record: journal
                .records
                .get(index)
                .ok_or_else(|| Error::Recovery("invalid journal record index".into()))?
                .clone(),
        };
        let checksum = update.checksum()?;
        let mut data = serde_json::to_vec(&Frame { update, checksum })
            .map_err(|e| Error::Recovery(e.to_string()))?;
        data.push(b'\n');

        filesystem::append_synced(path, &data)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    staging: String,
    index: usize,
    record: Record,
}

impl Update {
    fn checksum(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).map_err(|e| Error::Recovery(e.to_string()))?;

        Ok(blake3::hash(&bytes).to_hex().to_string())
    }

    fn replay(self, journal: &mut Journal) -> Result<()> {
        if self.staging != journal.staging || journal.complete {
            return Err(Error::Recovery(
                "journal update belongs to another run or a completed snapshot".into(),
            ));
        }

        if self.index == journal.records.len() && self.record.state == State::Prepared {
            journal.records.push(self.record);
            return Ok(());
        }

        let previous = journal
            .records
            .get_mut(self.index)
            .ok_or_else(|| Error::Recovery("out-of-order journal record".into()))?;
        let valid = matches!(
            (previous.state, self.record.state),
            (
                State::Prepared,
                State::Done | State::Undoing | State::Undone
            ) | (State::Done, State::Undoing)
                | (State::Undoing, State::Undoing | State::Undone)
        );
        if previous.operation != self.record.operation || !valid {
            return Err(Error::Recovery("invalid journal record transition".into()));
        }

        previous.state = self.record.state;

        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    update: Update,
    checksum: String,
}

fn decode(data: &[u8]) -> Result<Journal> {
    let mut snapshots = serde_json::Deserializer::from_slice(data).into_iter::<Journal>();
    let mut journal = snapshots
        .next()
        .transpose()
        .map_err(|e| Error::Recovery(e.to_string()))?
        .ok_or_else(|| Error::Recovery("empty journal".into()))?;
    let tail = &data[snapshots.byte_offset()..];
    if tail.iter().all(u8::is_ascii_whitespace) {
        return Ok(journal);
    }

    if journal.version != 2 || !tail.starts_with(b"\n") {
        return Err(Error::Recovery(
            "unexpected data after journal snapshot".into(),
        ));
    }

    for line in tail[1..].split_inclusive(|&b| b == b'\n') {
        // A mutation only follows a complete, flushed intent. An unterminated
        // final append cannot authorize one; recovery uses the preceding state.
        if !line.ends_with(b"\n") {
            if !line.starts_with(b"{") {
                return Err(Error::Recovery("invalid journal update".into()));
            }

            break;
        }

        let frame: Frame =
            serde_json::from_slice(line).map_err(|e| Error::Recovery(e.to_string()))?;
        if frame.checksum != frame.update.checksum()? {
            return Err(Error::Recovery("journal update checksum mismatch".into()));
        }

        frame.update.replay(&mut journal)?;
    }

    Ok(journal)
}

pub fn pending(root: &Path) -> PathBuf {
    root.join(".fortify/pending-run.json")
}

pub fn last(root: &Path) -> PathBuf {
    root.join(".fortify/last-run.json")
}

fn single_name(name: &str, prefix: &str) -> bool {
    name.starts_with(prefix)
        && !name.contains(['/', '\\', ':'])
        && name.len() > prefix.len()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn read(path: &Path, root: &Path) -> Result<Option<Journal>> {
    let Some(meta) = filesystem::metadata(path)? else {
        return Ok(None);
    };

    if filesystem::is_link(&meta) || !meta.is_file() {
        return Err(Error::Recovery(format!(
            "journal is not a regular file: {}",
            path.display()
        )));
    }

    let data = fs::read(path).context("read journal")?;
    let journal = decode(&data)
        .map_err(|e| Error::Recovery(format!("invalid journal {}: {e}", path.display())))?;

    if !matches!(journal.version, 1 | 2)
        || journal.root != root
        || journal.root_identity != filesystem::identity(root)?
    {
        return Err(Error::Recovery(
            "journal version or target identity does not match".into(),
        ));
    }

    if !single_name(&journal.staging, "run-") {
        return Err(Error::Recovery("invalid journal staging name".into()));
    }

    for record in &journal.records {
        match &record.operation {
            Operation::Move {
                from, to, digest, ..
            } => {
                filesystem::validate_relative(from)?;
                filesystem::validate_relative(to)?;
                if from == to
                    || digest.len() != 64
                    || !digest.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(Error::Recovery("invalid move record".into()));
                }
            }

            Operation::Directory { path, slot, .. } => {
                filesystem::validate_relative(path)?;
                if !single_name(slot, "dir-") {
                    return Err(Error::Recovery("invalid directory staging slot".into()));
                }
            }
        }
    }

    Ok(Some(journal))
}

pub fn staging_path(journal: &Journal) -> Result<PathBuf> {
    let base = filesystem::internal_dir(&journal.root)?.join(&journal.staging);
    if let Some(meta) = filesystem::metadata(&base)?
        && (filesystem::is_link(&meta) || !meta.is_dir())
    {
        return Err(Error::Recovery("unsafe journal staging directory".into()));
    }

    Ok(base)
}

pub fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => filesystem::sync_parent(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(format!("remove {}", path.display()), e)),
    }
}

// Staging holds empty directories only. Never recursively delete it.
pub fn cleanup(journal: &Journal) -> Result<()> {
    let base = staging_path(journal)?;

    for record in &journal.records {
        if let Operation::Directory { slot, identity, .. } = &record.operation {
            let path = base.join(slot);
            if let Some(meta) = filesystem::metadata(&path)? {
                if filesystem::is_link(&meta)
                    || !meta.is_dir()
                    || filesystem::identity(&path)? != *identity
                {
                    return Err(Error::Recovery(format!(
                        "staging directory changed: {}",
                        path.display()
                    )));
                }

                fs::remove_dir(&path)
                    .context(format!("remove empty staging directory {}", path.display()))?;
            }
        }
    }

    match fs::remove_dir(&base) {
        Ok(()) => filesystem::sync_parent(&base),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(
            format!("remove empty staging directory {}", base.display()),
            e,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Journal {
        Journal {
            version: 2,
            root: PathBuf::from("target"),
            root_identity: Identity {
                volume: 1,
                index: 1,
            },
            staging: "run-test".into(),
            complete: false,
            records: vec![],
        }
    }

    fn update(state: State) -> Update {
        Update {
            staging: "run-test".into(),
            index: 0,
            record: Record {
                operation: Operation::Move {
                    from: "file.pdf".into(),
                    to: "Documents/file.pdf".into(),
                    stamp: Stamp {
                        identity: Identity {
                            volume: 1,
                            index: 2,
                        },
                        size: 4,
                        modified_ns: 1000,
                    },
                    digest: "a".repeat(64),
                },
                state,
            },
        }
    }

    fn snapshot_bytes(journal: &Journal) -> Vec<u8> {
        let mut data = serde_json::to_vec(journal).unwrap();
        data.push(b'\n');

        data
    }

    fn frame_bytes(update: Update) -> Vec<u8> {
        let checksum = update.checksum().unwrap();
        let mut data = serde_json::to_vec(&Frame { update, checksum }).unwrap();
        data.push(b'\n');

        data
    }

    #[test]
    fn every_torn_append_uses_only_the_preceding_durable_state() {
        let mut prefix = snapshot_bytes(&snapshot());

        for state in [State::Prepared, State::Done] {
            let frame = frame_bytes(update(state));

            for length in 0..frame.len() {
                let mut torn = prefix.clone();
                torn.extend_from_slice(&frame[..length]);
                let recovered = decode(&torn).unwrap();

                if state == State::Prepared {
                    assert!(recovered.records.is_empty());
                } else {
                    assert_eq!(recovered.records[0].state, State::Prepared);
                }
            }

            prefix.extend_from_slice(&frame);
            assert_eq!(decode(&prefix).unwrap().records[0].state, state);
        }
    }

    #[test]
    fn complete_corrupt_frames_are_rejected() {
        let frame = frame_bytes(update(State::Prepared));
        let mut value: serde_json::Value = serde_json::from_slice(&frame).unwrap();
        value["update"]["record"]["operation"]["digest"] = "b".repeat(64).into();
        let mut data = snapshot_bytes(&snapshot());
        data.extend(serde_json::to_vec(&value).unwrap());
        data.push(b'\n');

        let error = decode(&data).unwrap_err();

        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn missing_reordered_and_foreign_records_are_rejected() {
        let mut foreign = update(State::Prepared);
        foreign.staging = "run-other".into();
        let mut missing = update(State::Prepared);
        missing.index = 1;

        for bad in [foreign, missing, update(State::Done)] {
            let mut data = snapshot_bytes(&snapshot());
            data.extend(frame_bytes(bad));

            assert!(decode(&data).is_err());
        }
    }

    #[test]
    fn updates_cannot_rewrite_operations_or_follow_completed_snapshots() {
        let mut journal = snapshot();
        journal.records.push(update(State::Prepared).record);
        let mut changed = update(State::Done);
        if let Operation::Move { to, .. } = &mut changed.record.operation {
            *to = "elsewhere.pdf".into();
        }
        let mut data = snapshot_bytes(&journal);
        data.extend(frame_bytes(changed));

        assert!(decode(&data).is_err());

        journal.complete = true;
        let mut data = snapshot_bytes(&journal);
        data.extend(frame_bytes(update(State::Done)));

        assert!(decode(&data).is_err());
    }

    #[test]
    fn legacy_snapshots_do_not_accept_unvalidated_trailing_updates() {
        let mut legacy = snapshot();
        legacy.version = 1;
        let mut data = snapshot_bytes(&legacy);

        assert_eq!(decode(&data).unwrap().version, 1);
        data.extend(frame_bytes(update(State::Prepared)));
        assert!(decode(&data).is_err());
    }
}
