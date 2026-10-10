use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("terminal engine: {0}")]
    Engine(String),
    #[error("pty: {0}")]
    Pty(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
