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
    /// YouTube rejected the signed-in session (HTTP 401, or a 403 saying "signed
    /// out"): the cookie has expired or was revoked. Never a network problem.
    #[error("YouTube Music rejected the sign-in (HTTP {0})")]
    Auth(u16),
}

impl Error {
    /// The account's sign-in was rejected ([`Error::Auth`]).
    pub fn is_auth(&self) -> bool {
        matches!(self, Error::Auth(_))
    }

    /// The server couldn't be reached or failed on its side (offline, DNS, TLS,
    /// timeouts, 5xx / 429): worth retrying later as is.
    pub fn is_network(&self) -> bool {
        match self {
            Error::Http(e) => match e {
                ureq::Error::StatusCode(code) => *code >= 500 || matches!(code, 408 | 429),
                ureq::Error::Io(_)
                | ureq::Error::Timeout(_)
                | ureq::Error::HostNotFound
                | ureq::Error::ConnectionFailed
                | ureq::Error::Tls(_)
                | ureq::Error::Protocol(_)
                | ureq::Error::BodyStalled
                | ureq::Error::ConnectProxyFailed(_) => true,
                _ => false,
            },
            Error::Io(_) => true,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
