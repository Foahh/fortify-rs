# Fortify

Organize your Downloads folder from the terminal. Fortify sorts files into categories, moves extra copies to `Duplicates`, and lets you undo the latest run.

Works on Windows, macOS, and Linux. Running `fortify` shows a preview; files move only when you apply it.

## Install

With stable Rust installed, run this from the repository:

```sh
cargo install --locked --path . --bins
```

This installs `fortify` and `fortify-runner`. Keep both in the same directory so scheduled runs can find the runner.

## Get started

```sh
fortify          # Preview your Downloads folder
fortify apply    # Organize files
fortify undo     # Undo the latest run
```

Fortify groups files into folders such as `Documents`, `Pictures`, and `Music`. It keeps the newest copy of duplicate files, gives conflicting filenames a numbered suffix, and removes empty category folders. Unrelated folders and existing subfolder layouts are left alone.

To use a different folder:

```sh
fortify preview -d "path/to/folder"
fortify apply -d "path/to/folder"
fortify undo -d "path/to/folder"
```

Add `--no-sort`, `--no-duplicates`, or `--no-prune` to preview or apply to skip that operation.

> [!IMPORTANT]
> Undo covers only the latest run that made changes, including scheduled runs.

## Customize

Create a configuration file, edit it, then check your changes:

```sh
fortify config init
fortify config validate
```

`config init` prints the file's location. Use `-c path/to/config.toml` to select another file, or `config init -d "path/to/folder"` to save a default target.

The [configuration guide](docs/configuration.md) explains categories, filename rules, and ignored files. The full default settings are in [config.toml](config.toml).

## Schedule

After saving your configuration, enable automatic organization:

```sh
fortify schedule enable --every 1h
fortify schedule status
fortify schedule disable
```

Use `--daily 09:00` instead of `--every 1h` for a daily run in local time. `schedule status` shows the target, configuration, and log location.

Scheduling uses Windows Task Scheduler, macOS LaunchAgents, or Linux systemd user timers. See [scheduling and recovery](docs/usage.md) for platform requirements and help with failed runs.

Use `fortify --help` for command options. Build and test instructions are in the [developer guide](CONTRIBUTING.md).
