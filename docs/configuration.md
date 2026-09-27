# Configuration

`fortify config init` creates a `config.toml` with the default categories and prints its location. It refuses to overwrite an existing file.

## File location

| Platform | Default configuration |
| --- | --- |
| Windows | `%APPDATA%\fortify\config\config.toml` |
| macOS | `~/Library/Application Support/fortify/config.toml` |
| Linux | `$XDG_CONFIG_HOME/fortify/config.toml`, or `~/.config/fortify/config.toml` |

To keep a configuration elsewhere:

```sh
fortify config init -c "path/to/config.toml"
fortify preview -c "path/to/config.toml"
```

Fortify uses the file passed with `-c`, then the default location, then its built-in settings. A selected file must exist and be valid. A file in the working directory is used only when passed with `-c`.

## Settings

This complete example sorts documents and pictures, ignores temporary files, and gives screenshots their own folder:

```toml
version = 1
sort = true
duplicates = true
prune = true
ignore = ["desktop.ini", "*.part", "*.crdownload", "*.tmp"]

[mapping]
Documents = ["pdf", "txt", "md"]
Pictures = ["jpg", "png"]

[[rules]]
name = "screenshots"
match = ["Screenshot*", "Screen Shot *"]
folder = "Pictures/Screenshots"
```

Set `sort`, `duplicates`, or `prune` to `false` to turn that operation off. All three default to `true` when omitted.

For a default target, add `directory` before `[mapping]`:

```toml
directory = 'E:\Downloads'
```

Use an absolute path for your platform. A relative path is resolved from the configuration file's folder; `~` and environment variables are not expanded. `config init -d "path/to/folder"` saves an absolute path for you.

A command's `-d` option overrides `directory`. With neither set, Fortify uses the OS Downloads folder.

## Matching files

Fortify chooses the destination in this order:

1. First matching `[[rules]]` entry.
2. Exact extension in `[mapping]`.
3. First matching wildcard extension in declaration order.
4. `Others`.

Named rules match the filename, including its extension, and are case-sensitive. Extension matching ignores case: `pdf` also matches `Report.PDF`. A wildcard such as `r0*` matches extensions such as `r01`.

Ignore patterns match filenames or paths relative to the target. Use `/` in path patterns, for example `Pictures/private/**`.

Destinations must stay inside the target. Absolute paths, `..`, `.fortify`, `Duplicates`, and platform-reserved filenames are rejected. Rule names must be unique, and an exact extension cannot belong to two categories.

A saved configuration supplies its own mappings and ignore list; it is not merged with the built-in defaults. Start with [config.toml](../config.toml) to keep all default categories.

## Validate or reset

```sh
fortify config validate
fortify config reset
```

Validation checks the configuration without requiring the target folder to exist.

Reset saves a `config-*.toml.bak` beside the existing file, then restores all defaults, including the target and operation settings. Use `-d` with reset to save a target again.
