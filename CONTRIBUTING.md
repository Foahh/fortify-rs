# Development

Fortify is one Rust 2024 Cargo package with a reusable library, a CLI, and a scheduler runner. Use the stable toolchain selected by `rust-toolchain.toml`.

## Build and check

```sh
cargo build --locked --release --bins
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

Release binaries are written to `target/release`, with `.exe` extensions on Windows. The runner uses the Windows GUI subsystem so scheduled runs do not open a console window.

CI checks Windows, macOS, and Linux and packages both binaries with the documentation and default configuration.

## Source layout

| Module | Responsibility |
| --- | --- |
| `config` | Parse settings, compile rules, and resolve defaults |
| `scan` / `plan` | Inspect files and build deterministic plans |
| `engine` / `journal` / `filesystem` | Execute operations, record recovery state, and access native filesystem APIs |
| `schedule` | Register and inspect native schedules |
| `cli` | Parse arguments and present results |

Keep console output in the CLI, filesystem mutations in the executor and filesystem adapter, and platform-specific code behind `cfg`.

Use blank lines between functions and logical steps. Keep templates readable and separate test setup, actions, and assertions.

## Safety requirements

Changes to execution must preserve these guarantees:

- Apply locks the target before scanning; undo uses the same persistent OS-backed lock.
- Moves recheck source identity and contents, stay inside the target, and never overwrite a destination.
- Journal intent is flushed before a mutation, and completion is persisted afterward.
- Undo refuses changed files or occupied original paths and can resume after interruption.
- A run with no target mutations preserves the existing undo record.
- Failed schedule updates attempt to restore the previous registration and report restoration failures.

Cover changes with temporary-directory tests and injected failure cases. Routine tests use fake scheduler backends and must not modify user schedules.

## Native scheduler smoke test

This optional test creates a temporary, uniquely named schedule, checks it, and removes it. Run it only in a disposable user session with the platform's scheduler available.

On macOS or Linux:

```sh
FORTIFY_NATIVE_SMOKE=1 cargo test --locked --features native-smoke --test native_smoke -- --ignored --nocapture
```

In PowerShell:

```powershell
$env:FORTIFY_NATIVE_SMOKE = "1"
try {
    cargo test --locked --features native-smoke --test native_smoke -- --ignored --nocapture
} finally {
    Remove-Item Env:FORTIFY_NATIVE_SMOKE
}
```

If cleanup fails, the test reports that its registration needs manual removal.
