# Fortify

Use Cargo and Rust tools in this repository.

- Run `cargo fmt --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, and `cargo test --locked --all-features` for code changes.
- Use blank lines between items, methods, and logical steps; separate test setup, actions, and assertions.
- Split long configuration fixtures and scheduler templates across readable lines.
- Keep filesystem mutation inside the executor and native filesystem adapter.
- Preserve no-overwrite moves, journal validation, and recovery checks.
- Use temporary targets and injected scheduler backends in routine tests. Never install a real user schedule during ordinary development or tests.
- Keep platform-specific code behind `cfg` and maintain the three-platform CI matrix.
- Document current behavior. Keep the README focused on installation and everyday use; put detailed reference material in `docs/` and developer guidance in `CONTRIBUTING.md`.
