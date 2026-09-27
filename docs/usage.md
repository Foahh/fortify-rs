# Usage and recovery

## Choose what to organize

`fortify` and `fortify preview` show the same read-only plan. `fortify apply` scans again before moving files.

Use `-d` / `--directory` to select a folder and `-c` / `--config` to select a configuration. Preview and apply also accept:

| Option | Effect |
| --- | --- |
| `--no-sort` | Skip category sorting |
| `--no-duplicates` | Leave duplicate files in place |
| `--no-prune` | Keep empty folders |

Fortify scans root files, configured category folders, and the paths leading to them. Other subfolders remain untouched. For example, `Pictures/Vacation` is left alone unless a rule names it as a destination.

`Duplicates`, `.fortify`, symbolic links, Windows junctions, and the active configuration file are excluded.

Duplicates must have identical contents. Fortify keeps the copy with the newest modification time; equal timestamps are resolved by relative path. Existing destination files are never overwritten: `report.pdf` becomes `report_1.pdf`, then `report_2.pdf` if needed. Moves across filesystems are refused.

## Undo and failed runs

```sh
fortify undo -d "path/to/folder"
```

With `-d`, undo works even if the configuration is missing or invalid. It restores moved files and empty folders removed during the run.

Only the latest run that made changes is available to undo. A run with no changes preserves that record. Scheduled runs use the same undo history.

Apply stops at the first error. If a run is partly complete, use undo to reverse its recorded operations before applying again.

| Problem | What to do |
| --- | --- |
| An original path is occupied | Move the conflicting entry elsewhere, then retry undo. |
| A moved file was edited | Preserve your edits separately. Undo requires the original file with its recorded contents and metadata. |
| A run was interrupted | Run undo. It resumes from recorded progress. |
| The target is locked | Let the other Fortify process finish, then retry. |
| The recorded state is ambiguous | Check the paths named in the error and restore the expected file or folder before retrying. |

Keep the target's `.fortify` folder intact while you need undo. It stores recovery information; deleting it discards that history. It is not a backup of file contents.

## Schedule automatic runs

Save a valid configuration and make sure the target exists before enabling a schedule:

```sh
fortify schedule enable --every 1h
fortify schedule status
```

Choose either `--every Nm|Nh|Nd`, from one minute through 31 days, or `--daily HH:MM` in local time. Enabling again updates the user's single schedule.

| Platform | Requirements and timing |
| --- | --- |
| Windows | Task Scheduler; the user must be logged in. Missed starts use its “start when available” setting. |
| macOS | A GUI login session. LaunchAgents follow launchd's interval and calendar behavior during sleep and wake. |
| Linux | An available systemd user manager. Daily timers catch up on missed runs; interval timers restart their interval when activated. |

Sleep, logout, and service availability can delay or skip runs. Fortify does not enable systemd lingering.

The schedule saves absolute executable, target, and configuration paths. Rule edits take effect on later runs. Enable the schedule again after moving the binaries or changing the target. On Windows, the executable's path must not contain `%`.

`schedule status` checks the native scheduler and prints the log location. Results and errors append to `fortify.log`; logs are not rotated automatically.

```sh
fortify schedule disable
```

Disabling retains your configuration and does not stop a run already in progress.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success, including no changes |
| `1` | An operation failed |
| `2` | Invalid arguments or configuration |
