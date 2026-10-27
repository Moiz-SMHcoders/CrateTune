use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct GetToken {
    pub token: String,
}

#[derive(Deserialize)]
pub struct GetSession {
    pub session: Session,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub name: String,
    pub key: String,
    pub subscriber: i8,
}
