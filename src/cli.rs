use crate::{
    Error, Result,
    config::{Config, DEFAULT_CONFIG, Operations},
    engine,
    error::IoContext,
    filesystem,
    paths::{self, AppPaths},
    plan::{Action, Plan},
    schedule::{self, Timing},
};
use clap::{Args, Parser, Subcommand};
use std::{fs, io::Write, path::PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "fortify",
    version,
    about = "Preview, organize, and undo file moves safely"
)]
pub struct Cli {
    #[arg(short, long, global = true)]
    pub directory: Option<PathBuf>,
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,
    #[command(flatten)]
    pub operations: Overrides,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Default, Args)]
pub struct Overrides {
    #[arg(long, global = true)]
    pub no_sort: bool,
    #[arg(long, global = true)]
    pub no_duplicates: bool,
    #[arg(long, global = true)]
    pub no_prune: bool,
}

impl Overrides {
    fn apply(&self, mut options: Operations) -> Operations {
        options.sort &= !self.no_sort;
        options.duplicates &= !self.no_duplicates;
        options.prune &= !self.no_prune;
        options
    }

    fn any(&self) -> bool {
        self.no_sort || self.no_duplicates || self.no_prune
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Display planned changes without modifying files
    Preview,
    /// Build a fresh plan, then execute it with undo protection
    Apply,
    /// Reverse the latest run or recover an interrupted run
    Undo,
    /// Manage the versioned TOML configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage native user scheduling
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Create configuration; existing files are never overwritten
    Init,
    /// Validate rules without requiring the target directory to exist
    Validate,
    /// Back up existing configuration, then restore all default settings
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCommand {
    /// Register or update one schedule for the current user
    Enable {
        #[arg(long, required_unless_present = "daily", conflicts_with = "daily")]
        every: Option<String>,
        #[arg(long, required_unless_present = "every", conflicts_with = "every")]
        daily: Option<String>,
    },
    /// Remove the schedule while retaining configuration
    Disable,
    /// Inspect the native scheduler and display saved paths
    Status,
}

pub fn entry() -> i32 {
    let cli = Cli::parse();

    match AppPaths::discover().and_then(|paths| run(cli, &paths)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error}");
            error.exit_code()
        }
    }
}

pub fn run(cli: Cli, app: &AppPaths) -> Result<()> {
    if cli.operations.any()
        && !matches!(cli.command, None | Some(Command::Preview | Command::Apply))
    {
        return Err(Error::Config(
            "--no-sort, --no-duplicates, and --no-prune apply only to preview/apply".into(),
        ));
    }

    match cli.command {
        Some(Command::Config {
            command: ConfigCommand::Init,
        }) => return write_config(&cli, app, false),
        Some(Command::Config {
            command: ConfigCommand::Reset,
        }) => return write_config(&cli, app, true),
        Some(Command::Config {
            command: ConfigCommand::Validate,
        }) => {
            let config = Config::load(cli.config.as_deref(), app)?;
            println!(
                "Configuration is valid: {}",
                config.source.as_deref().map_or("embedded defaults", |p| p
                    .to_str()
                    .unwrap_or("(Unicode path)"))
            );
            return Ok(());
        }

        Some(Command::Schedule {
            command: ScheduleCommand::Disable,
        }) => {
            let mut backend = schedule::native()?;
            schedule::disable(app, backend.as_mut())?;
            println!("Schedule disabled. Configuration retained.");
            return Ok(());
        }

        Some(Command::Schedule {
            command: ScheduleCommand::Status,
        }) => {
            let mut backend = schedule::native()?;
            let status = backend.snapshot()?;
            println!(
                "Schedule: {}",
                if status.enabled {
                    "active"
                } else if status.primary.is_some() {
                    "inactive"
                } else {
                    "not registered"
                }
            );
            if let Some(spec) = schedule::read_spec(app)? {
                println!(
                    "Target: {}\nConfig: {}\nRunner: {}\nTiming: {}\nLog: {}",
                    spec.directory.display(),
                    spec.config.display(),
                    spec.runner.display(),
                    spec.timing,
                    spec.log.display()
                );
                if !status.enabled {
                    println!("Saved configuration exists, but the native schedule is inactive.");
                }
            }

            return Ok(());
        }

        _ => {}
    }

    if matches!(cli.command, Some(Command::Undo))
        && let Some(target) = &cli.directory
    {
        println!(
            "Undo complete: {} operation(s) reversed.",
            engine::undo(target)?
        );
        return Ok(());
    }

    let config = Config::load(cli.config.as_deref(), app)?;
    let target = config.target(cli.directory.as_deref())?;
    let options = cli.operations.apply(config.operations());

    match cli.command {
        None | Some(Command::Preview) => {
            let plan = engine::preview(&target, &config, options)?;
            print_plan(&plan);
            if !plan.actions.is_empty() {
                println!("\nRun fortify apply with the same options to execute a fresh plan.");
            }
        }

        Some(Command::Apply) => {
            let report = engine::apply(&target, &config, options)?;
            print_plan(&report.plan);
            println!("Applied {} operation(s).", report.completed);
        }

        Some(Command::Undo) => {
            println!(
                "Undo complete: {} operation(s) reversed.",
                engine::undo(&target)?
            );
        }

        Some(Command::Schedule {
            command: ScheduleCommand::Enable { every, daily },
        }) => {
            let timing = if let Some(every) = every {
                Timing::every(&every)?
            } else {
                Timing::daily(daily.as_deref().unwrap_or(""))?
            };

            let source = config.source.as_ref().ok_or_else(|| {
                Error::Config(
                    "scheduling requires a saved configuration; run fortify config init first"
                        .into(),
                )
            })?;
            let spec = schedule::Spec::new(source, &target, app, timing)?;
            let mut backend = schedule::native()?;
            schedule::enable(app, backend.as_mut(), &spec)?;
            println!(
                "Schedule enabled: {}\nTarget: {}\nLog: {}",
                spec.timing,
                spec.directory.display(),
                spec.log.display()
            );
        }

        _ => unreachable!("configuration and schedule administration handled above"),
    }

    Ok(())
}

fn write_config(cli: &Cli, app: &AppPaths, reset: bool) -> Result<()> {
    let destination = paths::absolute(cli.config.as_deref().unwrap_or(&app.config))?;
    let mut raw = Config::parse(DEFAULT_CONFIG, None)?.raw;
    if let Some(directory) = &cli.directory {
        raw.directory = Some(paths::absolute(directory)?);
    }

    let content = if cli.directory.is_some() {
        toml::to_string_pretty(&raw).map_err(|e| Error::Config(e.to_string()))?
    } else {
        DEFAULT_CONFIG.to_owned()
    };

    Config::parse(&content, Some(destination.clone()))?;
    let parent = destination
        .parent()
        .ok_or_else(|| Error::Config("config has no parent directory".into()))?;
    fs::create_dir_all(parent).context("create configuration directory")?;
    if reset {
        if let Some(meta) = filesystem::metadata(&destination)? {
            if filesystem::is_link(&meta) || !meta.is_file() {
                return Err(Error::Config("configuration is not a regular file".into()));
            }

            let old = fs::read(&destination).context("read previous configuration")?;
            let backup = tempfile::Builder::new()
                .prefix("config-")
                .suffix(".toml.bak")
                .tempfile_in(parent)
                .context("create configuration backup")?;
            let (mut backup_file, backup_path) = backup
                .keep()
                .map_err(|e| Error::io("keep configuration backup", e.error))?;
            backup_file
                .write_all(&old)
                .context("write configuration backup")?;
            backup_file
                .sync_all()
                .context("flush configuration backup")?;
            println!("Backup: {}", backup_path.display());
        }

        filesystem::atomic_write(&destination, content.as_bytes())?;
    } else {
        filesystem::create_new(&destination, content.as_bytes())?;
    }

    println!("Configuration: {}", destination.display());

    Ok(())
}

pub fn print_plan(plan: &Plan) {
    println!(
        "Plan: {} action(s) for {}",
        plan.actions.len(),
        plan.target.display()
    );

    for action in &plan.actions {
        match action {
            Action::Move {
                from, to, reason, ..
            } => println!("{reason}: {} -> {}", from.display(), to.display()),
            Action::RemoveDir { path, .. } => println!("prune: {}", path.display()),
        }
    }

    if plan.actions.is_empty() {
        println!("Nothing to do.");
    }
}

#[derive(Parser)]
#[command(name = "fortify-runner", about = "Internal native scheduler runner")]
struct RunnerArgs {
    #[arg(long, conflicts_with_all = ["config", "directory", "log"])]
    request: Option<String>,
    #[arg(long, required_unless_present = "request")]
    config: Option<PathBuf>,
    #[arg(long, required_unless_present = "request")]
    directory: Option<PathBuf>,
    #[arg(long, required_unless_present = "request")]
    log: Option<PathBuf>,
}

pub fn runner_entry() -> i32 {
    let args = match RunnerArgs::try_parse() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    let args = if let Some(encoded) = args.request {
        match schedule::RunnerRequest::decode(&encoded) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("{error}");
                return error.exit_code();
            }
        }
    } else {
        schedule::RunnerRequest {
            version: 1,
            config: args
                .config
                .expect("clap requires --config without --request"),
            directory: args
                .directory
                .expect("clap requires --directory without --request"),
            log: args.log.expect("clap requires --log without --request"),
        }
    };

    let result = (|| -> Result<()> {
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&args.log)
            .context(format!("open log {}", args.log.display()))?;
        let now = time::OffsetDateTime::now_utc();
        let outcome = (|| {
            let app = AppPaths::discover()?;
            let config = Config::load(Some(&args.config), &app)?;
            engine::apply(&args.directory, &config, config.operations())
        })();

        match outcome {
            Ok(report) => {
                writeln!(
                    log,
                    "{now}: applied {} operation(s) to {}",
                    report.completed,
                    args.directory.display()
                )
                .context("write scheduled result")?;
                log.sync_all().context("flush log")?;

                Ok(())
            }

            Err(error) => {
                writeln!(log, "{now}: {error}").context("write scheduled error")?;
                log.sync_all().context("flush log")?;

                Err(error)
            }
        }
    })();

    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            error.exit_code()
        }
    }
}
