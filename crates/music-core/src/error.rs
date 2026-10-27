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
}

pub type Result<T> = std::result::Result<T, Error>;
