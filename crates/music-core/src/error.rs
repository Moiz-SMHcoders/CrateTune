#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Http(#[from] ureq::Error),
    #[error("unexpected response: {0}")]
    Parse(String),
    #[error("not playable: {0}")]
    Unplayable(String),
    #[error("audio output: {0}")]
    Audio(String),
    /// Local storage: the library database, backups, tags of local files.
    #[error("storage: {0}")]
    Storage(String),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
