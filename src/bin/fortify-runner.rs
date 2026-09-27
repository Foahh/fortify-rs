#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    std::process::exit(fortify::cli::runner_entry());
}
