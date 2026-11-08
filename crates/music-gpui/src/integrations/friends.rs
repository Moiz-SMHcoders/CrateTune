//! Friend groups and the activity feed in the app (BACKLOG M9; the model, the
//! invitations and the security notes are `music_core::friends`, the network side
//! rides on the device hand-off node, `integrations/devices.rs`).
//!
//! Off until switched on in Settings → Friends; sharing what you listen to is a second
//! switch, and a private session pauses it until the app quits. While on, the node
//! runs (with groups, or an invitation shown / being entered), the app mirrors what
//! plays into it when the song or play / pause changes (it goes out to the members'
//! devices then, not on a timer), and the feed is what arrived this run, in memory
//! only. "Listen together" on a group joins the group's session if a friend runs one,
//! else starts one (in the group's mode, Friends unless changed; only the group gets
//! the link, no room code) and sends its link to the members' devices: they get a
//! notification with Join (or join by themselves, "Join automatically when a friend
//! starts"), and the Together page lists the live sessions per group. When the session
//! ends its starter says so; a session that moves to another device (a Friends
//! session's hand-over) is announced again from there.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Context, Window};
use music_core::friends::{Activity, Group, Identity};
use music_core::handoff::{FriendsSetup, PairError, PairInput, PairingOffer, now_unix_ms};
use music_core::{PlaybackState, PlaybackStatus};

use music_core::together::{JoinRequest, SessionMode};

use super::config::{FriendHint, FriendsPrefs, HostMode};
use super::devices::PairStatus;
use super::together::{Role, StartOptions};
use crate::app::{MusicApp, notify};

/// A started session's link is sent to the group if hosting starts within this long.
const LISTEN_WINDOW: Duration = Duration::from_secs(30);
/// Leaving a group: it is kept this long for the "I left" record to go out.
const LEAVE_LINGER: Duration = Duration::from_secs(5);
/// "Listen together" invitations kept.
const MAX_INVITES: usize = 8;
/// A member whose device turned up hears about the group's running session again,
/// at most this often.
const REANNOUNCE_EVERY: Duration = Duration::from_secs(60);

/// A friend's "listen together" invitation.
#[derive(Debug, Clone, PartialEq)]
pub struct ListenInvite {
    pub member: String,
    pub group: String,
    pub link: String,
    pub at_ms: u64,
    /// The session's mode, when the link says it.
    pub mode: Option<SessionMode>,
    /// The session's id (the same session announced again from another device has it).
    pub session: Option<String>,
}

impl ListenInvite {
    pub fn new(member: String, group: String, link: String, at_ms: u64) -> Self {
        let mode = link
            .split(['?', '&'])
            .find_map(|p| p.strip_prefix("mode="))
            .and_then(SessionMode::parse);
        let session = match JoinRequest::parse(&link) {
            Some(JoinRequest::Direct(join)) => Some(join.session_id),
            _ => None,
        };
        Self {
            member,
            group,
            link,
            at_ms,
            mode,
            session,
        }
    }
}

/// A friend group's running session, as the Together page lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSession {
    pub group: String,
    pub group_name: String,
    /// Who announced it (the member's name).
    pub starter: String,
    pub link: String,
    pub mode: SessionMode,
    /// This device is in it.
    pub here: bool,
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
    /// Id, name, members, the mode its sessions start in.
    pub groups: Vec<(String, String, usize, SessionMode)>,
    pub feed: Vec<FeedRow>,
    /// Running sessions of the groups, newest first.
    pub sessions: Vec<GroupSession>,
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
    /// The group session this device announced (group, link): "ended" goes out when
    /// it ends here.
    announced: Option<(String, String)>,
    /// When each member last heard about the running session again.
    reannounced: HashMap<String, Instant>,
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

    /// "Listen together" on a group: joins the group's running session (a friend's,
    /// or this device's own: invites the members again), else starts one in the
    /// group's mode, only for the group.
    pub fn friends_listen_together(
        &mut self,
        group: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.integrations.together.group() == Some(group)
            && self.integrations.together.in_session()
        {
            if let Role::Hosting(host) = &self.integrations.together.role
                && let Some(node) = self.integrations.devices.node()
            {
                node.listen_together(group, &host.friends_link());
                notify(window, cx, t!("friends.toast.listen_sent"));
            }
            return;
        }
        let running = self
            .integrations
            .friends
            .invites
            .iter()
            .rev()
            .find(|i| i.group == group)
            .map(|i| i.link.clone());
        if let Some(link) = running {
            self.friends_join_session(&link, window, cx);
            return;
        }
        let mode = self.friends_prefs(cx).group_mode(group);
        let options = StartOptions {
            mode,
            group: Some(group.to_owned()),
            // Friends may be anywhere: through the relay (the LAN when unreachable).
            network: HostMode::Relay,
        };
        self.friends_start_session(group, options, window, cx);
    }

    /// Starts a session for a group (its link goes to the members once it is up).
    pub(crate) fn friends_start_session(
        &mut self,
        group: &str,
        options: StartOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.integrations.friends.pending_listen = Some((group.to_owned(), Instant::now()));
        self.together_start(options, window, cx, |_, _| {});
        notify(window, cx, t!("friends.toast.listen_sent"));
    }

    /// What the Together page's friends card shows.
    pub(crate) fn friends_panel(&self, cx: &App) -> FriendsPanel {
        let prefs = self.friends_prefs(cx);
        let state = &self.integrations.friends;
        let here_group = self
            .integrations
            .together
            .in_session()
            .then(|| self.integrations.together.group())
            .flatten();
        let mut sessions: Vec<GroupSession> = state
            .invites
            .iter()
            .rev()
            .filter_map(|i| {
                let group = prefs.groups.iter().find(|g| g.id() == i.group)?;
                Some(GroupSession {
                    group: i.group.clone(),
                    group_name: group.name().to_owned(),
                    starter: group
                        .record(&i.member)
                        .map(|m| m.name.clone())
                        .unwrap_or_default(),
                    link: i.link.clone(),
                    mode: i.mode.unwrap_or(SessionMode::Friends),
                    here: here_group == Some(i.group.as_str()),
                })
            })
            .collect();
        // This device's own group session (it announced it).
        if let (Some(group), Role::Hosting(host)) = (here_group, &self.integrations.together.role)
            && !sessions.iter().any(|s| s.group == group)
            && let Some(g) = prefs.groups.iter().find(|g| g.id() == group)
        {
            sessions.insert(
                0,
                GroupSession {
                    group: group.to_owned(),
                    group_name: g.name().to_owned(),
                    starter: String::new(),
                    link: host.link(),
                    mode: host.room.mode,
                    here: true,
                },
            );
        }
        FriendsPanel {
            enabled: prefs.enabled,
            sharing: prefs.share && !state.private_session,
            groups: prefs
                .groups
                .iter()
                .map(|g| {
                    (
                        g.id().to_owned(),
                        g.name().to_owned(),
                        g.current().len(),
                        prefs.group_mode(g.id()),
                    )
                })
                .collect(),
            feed: self.friends_feed(cx),
            sessions,
        }
    }

    /// A friend's "listen together": join their session.
    pub fn friends_join_session(
        &mut self,
        link: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self
            .integrations
            .friends
            .invites
            .iter()
            .find(|i| i.link == link)
            .map(|i| i.group.clone());
        self.together_join_for(link, group, window, cx);
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
                let link = host.friends_link();
                if let Some(node) = self.integrations.devices.node() {
                    node.listen_together(&group, &link);
                }
                self.integrations.friends.pending_listen = None;
                self.integrations.friends.announced = Some((group, link));
            }
        }
        // A group session that runs here now (taken over): announced from here.
        if let Role::Hosting(host) = &self.integrations.together.role
            && let Some(group) = &host.group
            && self.integrations.friends.announced.as_ref().map(|(_, l)| l)
                != Some(&host.friends_link())
            && self.integrations.friends.pending_listen.is_none()
        {
            self.integrations.friends.pending_listen = Some((group.clone(), Instant::now()));
        }
    }

    /// The group session this device announced is over here (`went_on`: it moved to
    /// another device, which announces it): tell the members unless it went on.
    pub(crate) fn friends_session_over(&mut self, went_on: bool) {
        let Some((group, link)) = self.integrations.friends.announced.take() else {
            return;
        };
        self.integrations.friends.reannounced.clear();
        if went_on {
            return;
        }
        if let Some(node) = self.integrations.devices.node() {
            node.listen_ended(&group, &link);
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

    pub(crate) fn friends_activity(
        &mut self,
        member: String,
        activity: Option<Activity>,
        cx: &mut Context<Self>,
    ) {
        // A member's device turned up while this device runs the group's session:
        // it hears about it (again).
        if let Some((group, link)) = self.integrations.friends.announced.clone()
            && self
                .friends_prefs(cx)
                .groups
                .iter()
                .any(|g| g.id() == group && g.is_member(&member))
        {
            let state = &mut self.integrations.friends;
            let due = state
                .reannounced
                .get(&member)
                .is_none_or(|at| at.elapsed() > REANNOUNCE_EVERY);
            if due {
                state.reannounced.insert(member.clone(), Instant::now());
                if let Some(node) = self.integrations.devices.node() {
                    node.listen_together(&group, &link);
                }
            }
        }
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
        // Heard before (announced again): no second notification.
        let known = invites.iter().any(|i| i.link == invite.link);
        // The same session from another device (a hand-over), or this member's older one.
        invites.retain(|i| {
            !(i.member == invite.member && i.group == invite.group)
                && (invite.session.is_none() || i.session != invite.session)
        });
        let link = invite.link.clone();
        let group_id = invite.group.clone();
        invites.push(invite);
        let extra = invites.len().saturating_sub(MAX_INVITES);
        invites.drain(..extra);
        cx.notify();
        if known {
            return;
        }
        // Already in it (this device was told too): nothing to do.
        if self.integrations.together.group() == Some(group_id.as_str())
            && self.integrations.together.in_session()
        {
            return;
        }
        let together = self.settings.read(cx).settings().together.clone();
        if together.auto_join && matches!(self.integrations.together.role, Role::Idle) {
            notify(
                window,
                cx,
                t!(
                    "friends.toast.auto_joining",
                    name = name,
                    group = group_name
                ),
            );
            self.together_join_for(&link, Some(group_id), window, cx);
            return;
        }
        self.friends_invite_note(
            t!(
                "friends.toast.listen_invite",
                name = name,
                group = group_name
            ),
            link,
            window,
            cx,
        );
    }

    /// The in-app notification for a friend's session, with Join.
    fn friends_invite_note(
        &mut self,
        message: String,
        link: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::{Sizable as _, WindowExt as _};
        let note = gpui_component::notification::Notification::new()
            .message(message)
            .action(move |_, _, _| {
                let link = link.clone();
                gpui_component::button::Button::new("friends-join-note")
                    .small()
                    .label(t!("friends.panel.join"))
                    .on_click(move |_, window, cx| {
                        let link = link.clone();
                        crate::app::update_app(cx, |app, cx| {
                            app.friends_join_session(&link, window, cx)
                        });
                    })
            });
        window.push_notification(note, cx);
    }

    /// A friend's session for a group is over (or they left it).
    pub(crate) fn friends_listen_ended(
        &mut self,
        member: &str,
        group: &str,
        link: &str,
        cx: &mut Context<Self>,
    ) {
        let session = ListenInvite::new(member.into(), group.into(), link.into(), 0).session;
        self.integrations.friends.invites.retain(|i| {
            !(i.group == group && (i.link == link || (session.is_some() && i.session == session)))
        });
        cx.notify();
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
