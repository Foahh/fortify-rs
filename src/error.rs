use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Operation(String),
    #[error("another fortify apply/undo is running for {0}")]
    Locked(PathBuf),
    #[error("recovery required: {0}")]
    Recovery(String),
    #[error(
        "{message}\nCompleted {completed} operation(s). Run fortify undo -d \"{target}\" to recover."
    )]
    Partial {
        message: String,
        completed: usize,
        target: PathBuf,
    },
    #[error("scheduler: {0}")]
    Scheduler(String),
}

impl Error {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub fn exit_code(&self) -> i32 {
        if matches!(self, Self::Config(_)) {
            2
        } else {
            1
        }
    }
}

pub trait IoContext<T> {
    fn context(self, context: impl Into<String>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::io(context, source))
    }
}
