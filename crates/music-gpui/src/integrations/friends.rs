//! Friend groups and the activity feed in the app (BACKLOG M9; the model, the
//! invitations and the security notes are `music_core::friends`, the network side
//! rides on the device hand-off node, `integrations/devices.rs`).
//!
//! Off until switched on in Settings → Friends; sharing what you listen to is a second
//! switch, and a private session pauses it until the app quits. While on, the node
//! runs (with groups, or an invitation shown / being entered), the app mirrors what
//! plays into it when the song or play / pause changes (it goes out to the members'
//! devices then, not on a timer), and the feed is what arrived this run, in memory
//! only. "Listen together" starts a Music Together session through the relay and
//! sends its link to the group's members, who see a Join button.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Context, Window};
use music_core::friends::{Activity, Group, Identity};
use music_core::handoff::{FriendsSetup, PairError, PairInput, PairingOffer, now_unix_ms};
use music_core::{PlaybackState, PlaybackStatus};

use super::config::{FriendHint, FriendsPrefs};
use super::devices::PairStatus;
use super::together::Role;
use crate::app::{MusicApp, notify};

/// A started session's link is sent to the group if hosting starts within this long.
const LISTEN_WINDOW: Duration = Duration::from_secs(30);
/// Leaving a group: it is kept this long for the "I left" record to go out.
const LEAVE_LINGER: Duration = Duration::from_secs(5);
/// "Listen together" invitations kept.
const MAX_INVITES: usize = 8;

/// A friend's "listen together" invitation.
#[derive(Debug, Clone, PartialEq)]
pub struct ListenInvite {
    pub member: String,
    pub group: String,
    pub link: String,
    pub at_ms: u64,
}

/// One line of the feed.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedRow {
    pub member: String,
    pub name: String,
    /// The groups shared with them (names).
    pub groups: Vec<String>,
    pub activity: Option<Activity>,
    /// When it arrived (Unix ms; 0: nothing heard this run).
    pub at_ms: u64,
}

/// The Together page's friends card (data only: the page draws it).
#[derive(Debug, Clone, Default)]
pub struct FriendsPanel {
    pub enabled: bool,
    pub sharing: bool,
    /// Id, name, members.
    pub groups: Vec<(String, String, usize)>,
    pub feed: Vec<FeedRow>,
    /// Newest first: the invitation, who sent it, the group's name.
    pub invites: Vec<(ListenInvite, String, String)>,
}

#[derive(Default)]
pub struct FriendsState {
    /// This installation's member id, once the key was read.
    pub member_id: Option<String>,
    /// Member id → what they listen to, and when that arrived.
    pub activity: HashMap<String, (Option<Activity>, u64)>,
    pub invites: Vec<ListenInvite>,
    /// The invitation shown: group id and code.
    pub invite: Option<(String, PairingOffer)>,
    /// Joining with a code typed here.
    pub join: PairStatus,
    /// Sharing paused until the app quits.
    pub private_session: bool,
    /// Asked for before the node was up.
    want_invite: Option<String>,
    want_join: Option<PairInput>,
    /// What the node was last given (to say hello when it changed).
    given: Option<u64>,
    /// What was last shared: (video id, playing) and when that song started.
    mirrored: Option<(Option<(String, bool)>, u64)>,
    /// "Listen together" asked for this group; its link goes out once hosting.
    pending_listen: Option<(String, Instant)>,
    /// Groups being left, with the "left" record (kept in the node a moment for it
    /// to go out).
    leaving: Vec<Group>,
    /// The settings changed: share (or stop sharing) what plays on the next poll.
    remirror: bool,
}

impl FriendsState {
    /// Whether the node is needed for friends now.
    pub(crate) fn busy(&self) -> bool {
        self.want_invite.is_some()
            || self.want_join.is_some()
            || self.invite.is_some()
            || self.join == PairStatus::Looking
    }
}

fn setup_hash(prefs: &FriendsPrefs) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&prefs.groups)
        .unwrap_or_default()
        .hash(&mut h);
    prefs.name.hash(&mut h);
    h.finish()
}

fn join_error_text(e: &PairError) -> String {
    match e {
        PairError::NotFound => t!("friends.join.not_found").into(),
        PairError::WrongCode => t!("friends.join.wrong_code").into(),
        PairError::NotPairing => t!("friends.join.not_inviting").into(),
        PairError::Network(e) => t!("friends.join.failed", error = e),
    }
}

/// The groups of `prefs` with `incoming` merged in (same id: merged; new: added).
pub(crate) fn merged_groups(mine: &[Group], incoming: &[Group]) -> Vec<Group> {
    let mut out = mine.to_vec();
    for theirs in incoming {
        match out.iter_mut().find(|g| g.id() == theirs.id()) {
            Some(g) => {
                g.merge(theirs);
            }
            None => out.push(theirs.clone()),
        }
    }
    out
}

/// The feed: every other current member of the groups, with what they play.
pub(crate) fn feed(
    prefs: &FriendsPrefs,
    me: Option<&str>,
    activity: &HashMap<String, (Option<Activity>, u64)>,
) -> Vec<FeedRow> {
    let mut rows: Vec<FeedRow> = Vec::new();
    for g in &prefs.groups {
        if me.is_some_and(|me| !g.is_member(me)) {
            continue;
        }
        for m in g.current() {
            if Some(m.member.as_str()) == me {
                continue;
            }
            match rows.iter_mut().find(|r| r.member == m.member) {
                Some(row) => row.groups.push(g.name().to_owned()),
                None => {
                    let (activity, at_ms) = activity.get(&m.member).cloned().unwrap_or_default();
                    rows.push(FeedRow {
                        member: m.member.clone(),
                        name: m.name.clone(),
                        groups: vec![g.name().to_owned()],
                        activity,
                        at_ms,
                    });
                }
            }
        }
    }
    // Playing first, then the latest news, then by name.
    rows.sort_by(|a, b| {
        let playing = |r: &FeedRow| r.activity.as_ref().is_some_and(|a| a.playing);
        playing(b)
            .cmp(&playing(a))
            .then(b.at_ms.cmp(&a.at_ms))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows
}

impl MusicApp {
    fn friends_prefs(&self, cx: &App) -> FriendsPrefs {
        self.settings.read(cx).settings().friends.clone()
    }

    /// Whether the node should run for friends.
    pub(crate) fn friends_wanted(&self, cx: &App) -> bool {
        let prefs = self.friends_prefs(cx);
        prefs.enabled && (!prefs.groups.is_empty() || self.integrations.friends.busy())
    }

    /// This installation's member key (made on first use; `None` until the keychain
    /// was read).
    fn friends_identity(&mut self, cx: &mut Context<Self>) -> Option<Identity> {
        let identity = self
            .settings
            .update(cx, |panel, cx| panel.friend_identity_or_create(cx))?;
        self.integrations.friends.member_id = Some(identity.id());
        Some(identity)
    }

    /// The device id (for a group record), from the device key.
    fn friends_device_id(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if let Some(id) = &self.integrations.devices.id {
            return Some(id.clone());
        }
        let key = self
            .settings
            .update(cx, |panel, cx| panel.device_key_or_create(cx))?;
        music_core::handoff::parse_device_key(&key).map(|k| music_core::handoff::device_id(&k))
    }

    fn friends_display_name(&self, cx: &App) -> String {
        let devices = self.devices_prefs(cx);
        self.friends_prefs(cx).display_name(&devices.name())
    }

    /// Gives the node who this is and the groups (or switches friends off in it).
    /// Called when the settings change and after the node started.
    pub(crate) fn friends_setup(&mut self, cx: &mut Context<Self>) {
        let prefs = self.friends_prefs(cx);
        if !prefs.enabled {
            if let Some(node) = self.integrations.devices.node() {
                node.set_friends(None);
            }
            let state = &mut self.integrations.friends;
            state.given = None;
            state.activity.clear();
            state.invite = None;
            return;
        }
        let Some(identity) = self.friends_identity(cx) else {
            return;
        };
        let name = self.friends_display_name(cx);
        let Some(node) = self.integrations.devices.node() else {
            return;
        };
        let hash = setup_hash(&prefs);
        let first = self.integrations.friends.given.is_none();
        let mut groups = prefs.groups.clone();
        // Groups being left stay a moment (their "left" record goes out).
        for left in &self.integrations.friends.leaving {
            if !groups.iter().any(|g| g.id() == left.id()) {
                groups.push(left.clone());
            }
        }
        self.integrations.friends.remirror = true;
        node.set_friends(Some(FriendsSetup {
            identity,
            name,
            groups,
        }));
        if first {
            let hints: Vec<_> = prefs
                .hints
                .iter()
                .map(|h| (h.id.clone(), h.relay_url.clone(), h.addrs.clone()))
                .collect();
            node.set_friend_hints(&hints);
        }
        if self.integrations.friends.given != Some(hash) {
            self.integrations.friends.given = Some(hash);
            node.friends_hello_all();
        }
        let state = &mut self.integrations.friends;
        if let Some(group) = state.want_invite.take() {
            state.invite = node.start_invite(&group).map(|o| (group, o));
        }
        if let Some(input) = state.want_join.take() {
            node.join_group(input);
        }
    }

    fn set_friends(&mut self, f: impl FnOnce(&mut FriendsPrefs), cx: &mut Context<Self>) {
        self.settings
            .update(cx, |panel, cx| panel.set_friends_prefs(f, cx));
    }

    /// Settings → Friends → "Create a group".
    pub fn friends_create_group(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = music_core::friends::group::clean_name(name);
        if name.is_empty() {
            return;
        }
        let (Some(identity), Some(device)) =
            (self.friends_identity(cx), self.friends_device_id(cx))
        else {
            notify(window, cx, t!("friends.toast.not_ready"));
            return;
        };
        let me = self.friends_display_name(cx);
        let group = Group::create(&identity, &name, &me, &device, now_unix_ms());
        self.set_friends(|f| f.groups.push(group), cx);
        notify(window, cx, t!("friends.toast.created", group = name));
    }

    /// Shows an invitation code for a group.
    pub fn friends_invite(&mut self, group: &str, cx: &mut Context<Self>) {
        let state = &mut self.integrations.friends;
        match self.integrations.devices.node() {
            Some(node) if state.given.is_some() => {
                state.invite = node.start_invite(group).map(|o| (group.to_owned(), o));
            }
            _ => {
                state.want_invite = Some(group.to_owned());
                self.devices_sync(cx);
                self.friends_setup(cx);
            }
        }
        self.notify_friends(cx);
    }

    pub fn friends_cancel_invite(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.integrations.friends;
        state.want_invite = None;
        if state.invite.take().is_some()
            && let Some(node) = self.integrations.devices.node()
        {
            node.cancel_invite();
        }
        self.devices_sync(cx);
        self.notify_friends(cx);
    }

    /// Settings → Friends → "Join": a code or an invitation link.
    pub fn friends_join(&mut self, input: &str, cx: &mut Context<Self>) {
        let Some(input) = PairInput::parse_invite(input) else {
            self.integrations.friends.join = PairStatus::Failed(t!("friends.join.bad_code").into());
            self.notify_friends(cx);
            return;
        };
        let state = &mut self.integrations.friends;
        state.join = PairStatus::Looking;
        match self.integrations.devices.node() {
            Some(node) if state.given.is_some() => node.join_group(input),
            _ => {
                state.want_join = Some(input);
                self.devices_sync(cx);
                self.friends_setup(cx);
            }
        }
        self.notify_friends(cx);
    }

    /// Leaves a group (the others are told; it is forgotten here).
    pub fn friends_leave(&mut self, group: &str, cx: &mut Context<Self>) {
        let prefs = self.friends_prefs(cx);
        let Some(identity) = self.friends_identity(cx) else {
            return;
        };
        let Some(mut left) = prefs.groups.iter().find(|g| g.id() == group).cloned() else {
            return;
        };
        let me = self.friends_display_name(cx);
        left.set_self(&identity, &me, None, true, now_unix_ms());
        let id = group.to_owned();
        self.integrations.friends.leaving.push(left);
        self.set_friends(|f| f.groups.retain(|g| g.id() != id), cx);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(LEAVE_LINGER).await;
            this.update(cx, |this, cx| {
                this.integrations.friends.leaving.retain(|g| g.id() != id);
                this.integrations.friends.given = None;
                this.friends_setup(cx);
                this.devices_sync(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The owner removes a member for good.
    pub fn friends_remove(&mut self, group: &str, member: &str, cx: &mut Context<Self>) {
        let Some(identity) = self.friends_identity(cx) else {
            return;
        };
        let (group, member) = (group.to_owned(), member.to_owned());
        self.set_friends(
            |f| {
                if let Some(g) = f.groups.iter_mut().find(|g| g.id() == group) {
                    let _ = g.remove(&identity, &member, now_unix_ms());
                }
            },
            cx,
        );
        self.integrations.friends.activity.remove(&member);
    }

    /// "Listen together": host a Music Together session through the relay; its link
    /// goes to the group's members once it is up.
    pub fn friends_listen_together(
        &mut self,
        group: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.integrations.friends.pending_listen = Some((group.to_owned(), Instant::now()));
        // Already hosting: the group is invited to that session (on the next poll).
        if !matches!(self.integrations.together.role, Role::Hosting(_)) {
            self.together_host_relay(window, cx, |_, _| {});
        }
        notify(window, cx, t!("friends.toast.listen_sent"));
    }

    /// What the Together page's friends card shows.
    pub(crate) fn friends_panel(&self, cx: &App) -> FriendsPanel {
        let prefs = self.friends_prefs(cx);
        let state = &self.integrations.friends;
        let invites = state
            .invites
            .iter()
            .rev()
            .map(|i| {
                let group = prefs.groups.iter().find(|g| g.id() == i.group);
                (
                    i.clone(),
                    group
                        .and_then(|g| g.record(&i.member))
                        .map(|m| m.name.clone())
                        .unwrap_or_default(),
                    group.map(|g| g.name().to_owned()).unwrap_or_default(),
                )
            })
            .collect();
        FriendsPanel {
            enabled: prefs.enabled,
            sharing: prefs.share && !state.private_session,
            groups: prefs
                .groups
                .iter()
                .map(|g| (g.id().to_owned(), g.name().to_owned(), g.current().len()))
                .collect(),
            feed: self.friends_feed(cx),
            invites,
        }
    }

    /// A friend's "listen together": join their session.
    pub fn friends_join_session(
        &mut self,
        link: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = link.to_owned();
        self.integrations.friends.invites.retain(|i| i.link != link);
        self.together_join(&link, window, cx);
    }

    pub fn friends_set_private(&mut self, on: bool, cx: &mut Context<Self>) {
        self.integrations.friends.private_session = on;
        self.integrations.friends.remirror = true;
        self.notify_friends(cx);
    }

    /// Replaces this installation's member key with the one of a recovery phrase. The
    /// groups of the old key are forgotten here (ask for invitations again).
    pub fn friends_restore(
        &mut self,
        phrase: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let identity = match Identity::from_recovery_phrase(phrase) {
            Ok(identity) => identity,
            Err(_) => {
                notify(window, cx, t!("friends.toast.bad_phrase"));
                return false;
            }
        };
        if self.integrations.friends.member_id.as_deref() == Some(identity.id().as_str()) {
            return true;
        }
        self.settings
            .update(cx, |panel, cx| panel.set_friend_identity(&identity, cx));
        self.integrations.friends = FriendsState {
            member_id: Some(identity.id()),
            ..FriendsState::default()
        };
        self.set_friends(|f| f.groups.clear(), cx);
        notify(window, cx, t!("friends.toast.restored"));
        true
    }

    fn notify_friends(&self, cx: &mut Context<Self>) {
        self.settings.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// From the poll: what plays here goes to the node when it changed, and a
    /// pending "listen together" link once the session is up.
    pub(crate) fn poll_friends(&mut self, state: &PlaybackState, cx: &mut Context<Self>) {
        if self.integrations.friends.given.is_none() {
            return;
        }
        let playing = state.status == PlaybackStatus::Playing;
        let current = state
            .track
            .as_ref()
            .filter(|t| !t.is_local() && music_core::together::is_shareable_id(&t.video_id))
            .map(|t| (t.video_id.clone(), playing));
        let key = self
            .integrations
            .friends
            .mirrored
            .as_ref()
            .map(|(k, _)| k.clone());
        if key != Some(current.clone()) || std::mem::take(&mut self.integrations.friends.remirror) {
            let started = match (&self.integrations.friends.mirrored, &current) {
                // Same song, play / pause: it started when it started.
                (Some((Some((id, _)), at)), Some((now_id, _))) if id == now_id => *at,
                _ => now_unix_ms().saturating_sub(state.position.as_millis() as u64),
            };
            self.integrations.friends.mirrored = Some((current.clone(), started));
            let share = self.friends_prefs(cx).share && !self.integrations.friends.private_session;
            let activity =
                state
                    .track
                    .as_ref()
                    .zip(current)
                    .filter(|_| share)
                    .map(|(t, (id, playing))| Activity {
                        video_id: id,
                        title: t.title.clone(),
                        artist: t.artists.clone(),
                        started_ms: started,
                        playing,
                    });
            self.friends_mirror(activity);
        }
        let pending = self.integrations.friends.pending_listen.clone();
        if let Some((group, since)) = pending {
            if since.elapsed() > LISTEN_WINDOW {
                self.integrations.friends.pending_listen = None;
            } else if let Role::Hosting(host) = &self.integrations.together.role {
                let link = host.link();
                if let Some(node) = self.integrations.devices.node() {
                    node.listen_together(&group, &link);
                }
                self.integrations.friends.pending_listen = None;
            }
        }
    }

    fn friends_mirror(&self, activity: Option<Activity>) {
        if let Some(node) = self.integrations.devices.node() {
            node.set_activity(activity);
        }
    }

    // ---- node events ---------------------------------------------------------------

    pub(crate) fn friends_groups_changed(
        &mut self,
        groups: Vec<Group>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let me = self.integrations.friends.member_id.clone();
        let prefs = self.friends_prefs(cx);
        let leaving = &self.integrations.friends.leaving;
        // Groups being left aren't taken back in.
        let incoming: Vec<Group> = groups
            .into_iter()
            .filter(|g| !leaving.iter().any(|l| l.id() == g.id()))
            .collect();
        let mut merged = merged_groups(&prefs.groups, &incoming);
        // Removed by the owner: forgotten, with a note.
        let mut removed = Vec::new();
        if let Some(me) = &me {
            merged.retain(|g| {
                let gone = g.removals.iter().any(|r| &r.member == me);
                if gone {
                    removed.push(g.name().to_owned());
                }
                !gone
            });
        }
        let next = FriendsPrefs {
            groups: merged,
            ..prefs.clone()
        };
        // The node has these already: no new hello for them.
        self.integrations.friends.given = Some(setup_hash(&next));
        if next.groups != prefs.groups {
            let groups = next.groups;
            self.set_friends(|f| f.groups = groups, cx);
        }
        for name in removed {
            notify(window, cx, t!("friends.toast.removed", group = name));
        }
    }

    pub(crate) fn friends_activity(&mut self, member: String, activity: Option<Activity>) {
        self.integrations
            .friends
            .activity
            .insert(member, (activity, now_unix_ms()));
    }

    pub(crate) fn friends_listen_invite(
        &mut self,
        invite: ListenInvite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prefs = self.friends_prefs(cx);
        let group = prefs.groups.iter().find(|g| g.id() == invite.group);
        let name = group
            .and_then(|g| g.record(&invite.member))
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let group_name = group.map(|g| g.name().to_owned()).unwrap_or_default();
        let invites = &mut self.integrations.friends.invites;
        invites.retain(|i| !(i.member == invite.member && i.group == invite.group));
        invites.push(invite);
        let extra = invites.len().saturating_sub(MAX_INVITES);
        invites.drain(..extra);
        notify(
            window,
            cx,
            t!(
                "friends.toast.listen_invite",
                name = name,
                group = group_name
            ),
        );
    }

    pub(crate) fn friends_joined(
        &mut self,
        group: Group,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = group.name().to_owned();
        self.integrations.friends.join = PairStatus::Paired(name.clone());
        self.friends_groups_changed(vec![group], window, cx);
        notify(window, cx, t!("friends.toast.joined", group = name));
    }

    pub(crate) fn friends_join_failed(&mut self, e: PairError, cx: &mut Context<Self>) {
        self.integrations.friends.join = PairStatus::Failed(join_error_text(&e));
        self.devices_sync(cx);
    }

    pub(crate) fn friends_invite_ended(
        &mut self,
        joined: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self
            .integrations
            .friends
            .invite
            .take()
            .map(|(g, _)| g)
            .and_then(|id| {
                self.friends_prefs(cx)
                    .groups
                    .iter()
                    .find(|g| g.id() == id)
                    .map(|g| g.name().to_owned())
            })
            .unwrap_or_default();
        if let Some(name) = joined {
            notify(
                window,
                cx,
                t!("friends.toast.member_joined", name = name, group = group),
            );
        }
        self.devices_sync(cx);
    }

    pub(crate) fn friends_device_seen(&mut self, hint: FriendHint, cx: &mut Context<Self>) {
        self.set_friends(
            |f| {
                f.hints.retain(|h| h.id != hint.id);
                f.hints.push(hint);
                let extra = f.hints.len().saturating_sub(FriendsPrefs::MAX_HINTS);
                f.hints.drain(..extra);
            },
            cx,
        );
    }

    /// The feed for the Together page and `@friends-status`.
    pub(crate) fn friends_feed(&self, cx: &App) -> Vec<FeedRow> {
        let prefs = self.friends_prefs(cx);
        feed(
            &prefs,
            self.integrations.friends.member_id.as_deref(),
            &self.integrations.friends.activity,
        )
    }

    /// `@friends-invite=GROUP:FILE`: writes the code and link once the node shows them.
    pub(crate) fn friends_write_invite(&mut self, path: String, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            for _ in 0..100 {
                let offer = this
                    .update(cx, |this, _| this.integrations.friends.invite.clone())
                    .ok()
                    .flatten();
                if let Some((_, offer)) = offer {
                    let written = std::fs::write(&path, &offer.code).and_then(|()| {
                        std::fs::write(format!("{path}.link"), offer.link.to_invite_link())
                    });
                    if let Err(e) = written {
                        eprintln!("music-gpui: writing {path}: {e}");
                    }
                    return;
                }
                executor.timer(Duration::from_millis(100)).await;
            }
            eprintln!("music-gpui: no invitation to write");
        })
        .detach();
    }

    /// `@friends-status`.
    pub(crate) fn print_friends_status(&self, cx: &App) {
        let prefs = self.friends_prefs(cx);
        let s = &self.integrations.friends;
        eprintln!(
            "music-gpui: friends enabled={} share={} private={} member={:?} groups={} invite={:?} join={:?}",
            prefs.enabled,
            prefs.share,
            s.private_session,
            s.member_id.as_deref().map(music_core::handoff::short_id),
            prefs.groups.len(),
            s.invite.as_ref().map(|(_, o)| o.code.as_str()),
            s.join
        );
        for g in &prefs.groups {
            let names: Vec<&str> = g.current().iter().map(|m| m.name.as_str()).collect();
            eprintln!("music-gpui: friends   group {:?}: {names:?}", g.name());
        }
        for row in self.friends_feed(cx) {
            eprintln!(
                "music-gpui: friends   {}: {:?}",
                row.name,
                row.activity
                    .as_ref()
                    .map(|a| (&a.title, &a.artist, a.playing))
            );
        }
        for i in &s.invites {
            eprintln!("music-gpui: friends   invite {} → {}", i.group, i.link);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feed_lists_other_members_playing_first() {
        let (ann, bob, cat) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let mut g = Group::create(&ann, "Friday", "Ann", "d-ann", 1);
        for (who, name) in [(&bob, "Bob"), (&cat, "Cat")] {
            g.vouch(&ann, &who.id(), 2).unwrap();
            g.members.retain(|_| true);
            g.set_self(who, name, Some(&format!("d-{name}")), false, 3);
        }
        let prefs = FriendsPrefs {
            groups: vec![g.clone()],
            ..FriendsPrefs::default()
        };
        let mut activity = HashMap::new();
        activity.insert(
            cat.id(),
            (
                Some(Activity {
                    title: "Song".into(),
                    playing: true,
                    ..Activity::default()
                }),
                10,
            ),
        );
        let rows = feed(&prefs, Some(&ann.id()), &activity);
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["Cat", "Bob"]
        );
        assert_eq!(rows[0].groups, ["Friday"]);
        // Not a member (any more): nothing.
        assert!(feed(&prefs, Some(&Identity::generate().id()), &activity).is_empty());
        // Merging copies keeps groups this side has and adds new ones.
        let other = Group::create(&bob, "Other", "Bob", "d-bob", 1);
        let merged = merged_groups(&prefs.groups, &[other.clone(), g]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1], other);
    }
}
