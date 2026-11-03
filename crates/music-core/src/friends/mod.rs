//! Friend groups and the activity feed (BACKLOG M9): lasting groups instead of
//! one-off Music Together rooms, and what your friends are listening to.
//!
//! **Identity.** Each installation has a member key ([`identity`], in the keychain);
//! friends know you by its public half. The recovery phrase moves it to a new
//! computer; that computer then joins your groups again with an invitation (it is
//! added as another device of the same member, keeping your name and, if you made
//! the group, your owner role).
//!
//! **Groups** ([`group`]) are signed records every member keeps a copy of; members
//! pass them on whenever they talk, so there is no server holding the list.
//!
//! **Invitations** reuse device pairing: a member shows a code (8 characters,
//! Crockford base32, plus a link with its device id and relay for other networks);
//! the newcomer types it, both run SPAKE2 keyed by the code over the iroh connection
//! and confirm the key (a wrong guess costs one of 5 tries and teaches nothing; the
//! code lasts 10 minutes and is used once). Only then does the inviter send the group
//! and vouch for the newcomer's member key, and the newcomer signs its own record.
//!
//! **Transport.** The device hand-off node's iroh endpoint, on ALPNs of its own
//! (`cratetune/friends/1` between members, `cratetune/invite/1` while a code is
//! shown): QUIC between the devices, end-to-end encrypted, directly when possible,
//! else through our relay, which only forwards encrypted bytes. A device answers
//! another only if that device belongs to a current member of a group they share,
//! and only ever sends a member the groups that member is in.
//!
//! **Activity** ([`Activity`]) is what is shared, and only while sharing is on and
//! no private session runs: the song's id, title and artist, when it started and
//! whether it plays, sent to the members' devices when it changes (no polling). It
//! is kept in memory only, on both ends.

pub mod group;
pub mod identity;

use serde::{Deserialize, Serialize};

pub use group::{Group, GroupError, MemberRecord};
pub use identity::{Identity, PhraseError};

/// Scheme of an invitation link (`music-friends://join?id=…&code=…&relay=…&addr=…`).
pub const INVITE_SCHEME: &str = "music-friends";
/// The mDNS user data of a device showing an invitation code.
pub const INVITING_USER_DATA: &str = "inviting";

/// What a member is listening to (all that is shared).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Activity {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    /// When the song started (Unix ms).
    pub started_ms: u64,
    pub playing: bool,
}

impl Activity {
    /// A copy with every field cut to a sane length (from the network).
    pub fn bounded(mut self) -> Self {
        let cut = |s: &mut String, n: usize| {
            if s.chars().count() > n {
                *s = s.chars().take(n).collect();
            }
        };
        cut(&mut self.video_id, 32);
        cut(&mut self.title, 200);
        cut(&mut self.artist, 200);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_wire_names_and_bounds() {
        let a = Activity {
            video_id: "abc".into(),
            title: "t".repeat(500),
            artist: "a".into(),
            started_ms: 5,
            playing: true,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains(r#""videoId":"abc""#) && json.contains(r#""startedMs":5"#));
        assert_eq!(a.bounded().title.len(), 200);
    }
}
