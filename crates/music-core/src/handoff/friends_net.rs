//! Friend groups on the hand-off node's endpoint (`crate::friends` has the model and
//! the security notes): hellos between members' devices (each side's copies of the
//! groups both are in, and what it is listening to), "listen together" invitations,
//! and joining a group with an invitation code (SPAKE2, as device pairing).
//!
//! Nothing here runs on a timer: hellos go out when friends are switched on, when a
//! group changed here and when what is shared changed (gathered for a moment), and
//! are answered when they come in. Friends' devices that can't be reached are simply
//! skipped; they say hello themselves when they start.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{EndpointAddr, EndpointId};

use super::messages::{FRIENDS_ALPN, Frame, INVITE_ALPN, Message, PeerInfo, code};
use super::node::{
    CONNECT_TIMEOUT, Command, Ctx, Event, FIND_TIMEOUT, PairError, PairingOffer, Pairing,
    REQUEST_TIMEOUT, ReadError, Shared, exchange, lock, now_unix_ms, read_frame, write_frame,
};
use super::pairing::{self, CODE_TTL, Exchange, MAX_ATTEMPTS, PairInput, PairingLink, Side};
use crate::friends::{Activity, Group, INVITING_USER_DATA, Identity, MemberRecord};

/// What is shared is gathered this long before it goes out (skipping through songs).
const PUSH_DELAY: Duration = Duration::from_millis(1500);

/// Who this is among friends (from the app's settings and the keychain).
#[derive(Clone)]
pub struct FriendsSetup {
    pub identity: Identity,
    /// The display name in the groups.
    pub name: String,
    pub groups: Vec<Group>,
}

impl std::fmt::Debug for FriendsSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FriendsSetup")
            .field("name", &self.name)
            .field("groups", &self.groups.len())
            .finish_non_exhaustive()
    }
}

pub(crate) struct FriendsShared {
    pub(super) setup: FriendsSetup,
    pub(super) activity: Option<Activity>,
    /// Where friends' devices were reachable.
    pub(super) hints: HashMap<EndpointId, (Option<String>, Vec<SocketAddr>)>,
    push_pending: bool,
}

impl FriendsShared {
    pub(super) fn new(setup: FriendsSetup) -> Self {
        Self {
            setup,
            activity: None,
            hints: HashMap::new(),
            push_pending: false,
        }
    }

    /// The devices of every other current member (of one group, or of all).
    fn devices(&self, group: Option<&str>) -> Vec<EndpointId> {
        let me = self.setup.identity.id();
        let mut out = HashSet::new();
        for g in &self.setup.groups {
            if group.is_some_and(|id| id != g.id()) || !g.is_member(&me) {
                continue;
            }
            for m in g.current() {
                if m.member != me {
                    out.extend(m.devices.iter().filter_map(|d| d.parse::<EndpointId>().ok()));
                }
            }
        }
        out.into_iter().collect()
    }

    /// The member a device belongs to, in any group shared with it.
    fn member_of(&self, device: &str) -> Option<String> {
        let me = self.setup.identity.id();
        self.setup
            .groups
            .iter()
            .filter(|g| g.is_member(&me))
            .find_map(|g| g.member_of_device(device).map(|m| m.member.clone()))
    }

    /// The hello for a device: the groups its member is in (or was removed from, so
    /// it learns that), and the activity.
    fn hello_for(&self, device: &str) -> Option<Message> {
        let groups: Vec<Group> = self
            .setup
            .groups
            .iter()
            .filter(|g| {
                g.members.iter().any(|m| {
                    m.devices.iter().any(|d| d == device)
                        && (g.is_member(&m.member)
                            || g.removals.iter().any(|r| r.member == m.member))
                })
            })
            .map(Group::shareable)
            .collect();
        (!groups.is_empty()).then(|| Message::FriendHello {
            groups,
            activity: self.activity.clone(),
        })
    }

    /// Takes in another member's copies of groups this device is in.
    fn merge(&mut self, groups: &[Group]) -> bool {
        let mut changed = false;
        for theirs in groups {
            if let Some(mine) = self
                .setup
                .groups
                .iter_mut()
                .find(|g| g.id() == theirs.id())
            {
                changed |= mine.merge(theirs);
            }
        }
        changed
    }
}

/// Friends commands of the node.
pub(crate) enum FriendsCommand {
    HelloAll,
    /// What is shared changed: tell everyone soon.
    Push,
    Advertise(bool),
    Join(PairInput),
    ListenTogether { group: String, link: String },
}

/// Starts showing an invitation code for `group`.
pub(super) fn start_invite(
    shared: &Shared,
    group: &str,
    id: String,
    (relay_url, addrs): (Option<String>, Vec<SocketAddr>),
) -> Option<PairingOffer> {
    {
        let friends = lock(&shared.friends);
        let f = friends.as_ref()?;
        let me = f.setup.identity.id();
        f.setup
            .groups
            .iter()
            .find(|g| g.id() == group)
            .filter(|g| g.is_member(&me))?;
    }
    let code = pairing::new_code();
    *lock(&shared.inviting) = Some((
        group.to_owned(),
        Pairing {
            code: code.clone(),
            until: Instant::now() + CODE_TTL,
            attempts: 0,
            done: false,
        },
    ));
    Some(PairingOffer {
        link: PairingLink {
            id,
            code: code.clone(),
            relay_url,
            addrs,
        },
        code,
    })
}

pub(super) fn handle(ctx: &Arc<Ctx>, command: FriendsCommand) {
    let ctx = ctx.clone();
    match command {
        FriendsCommand::HelloAll => hello_all(&ctx, None),
        FriendsCommand::Push => {
            let due = match lock(&ctx.shared.friends).as_mut() {
                Some(f) if !f.push_pending => {
                    f.push_pending = true;
                    true
                }
                _ => false,
            };
            if due {
                tokio::spawn(async move {
                    tokio::time::sleep(PUSH_DELAY).await;
                    if let Some(f) = lock(&ctx.shared.friends).as_mut() {
                        f.push_pending = false;
                    }
                    hello_all(&ctx, None);
                });
            }
        }
        FriendsCommand::Advertise(on) => {
            let data = on
                .then(|| INVITING_USER_DATA.parse::<iroh::address_lookup::UserData>().ok())
                .flatten();
            ctx.endpoint.set_user_data_for_address_lookup(data);
            if on {
                tokio::spawn(async move {
                    tokio::time::sleep(CODE_TTL).await;
                    let expired = {
                        let mut inviting = lock(&ctx.shared.inviting);
                        let gone = inviting.as_ref().is_some_and(|(_, p)| p.until <= Instant::now());
                        if gone {
                            *inviting = None;
                        }
                        gone
                    };
                    if expired {
                        ctx.endpoint.set_user_data_for_address_lookup(None);
                        ctx.emit(Event::InviteEnded { joined: None });
                    }
                });
            }
        }
        FriendsCommand::Join(input) => {
            tokio::spawn(async move {
                match join(&ctx, input).await {
                    Ok(group) => ctx.emit(Event::Joined(group)),
                    Err(e) => ctx.emit(Event::JoinFailed(e)),
                }
            });
        }
        FriendsCommand::ListenTogether { group, link } => {
            let devices = match lock(&ctx.shared.friends).as_ref() {
                Some(f) => f.devices(Some(&group)),
                None => return,
            };
            for device in devices {
                let ctx = ctx.clone();
                let msg = Message::ListenTogether {
                    group: group.clone(),
                    link: link.clone(),
                };
                tokio::spawn(async move {
                    if let Err(e) = request(&ctx, device, &msg).await {
                        log::debug!("friends: {} not reached ({e})", device.fmt_short());
                    }
                });
            }
        }
    }
}

/// A hello to every other member's devices (of one group, or of all).
fn hello_all(ctx: &Arc<Ctx>, group: Option<&str>) {
    let devices = match lock(&ctx.shared.friends).as_ref() {
        Some(f) => f.devices(group),
        None => return,
    };
    for device in devices {
        let ctx = ctx.clone();
        tokio::spawn(async move { hello(&ctx, device).await });
    }
}

async fn hello(ctx: &Arc<Ctx>, device: EndpointId) {
    let msg = match lock(&ctx.shared.friends).as_ref() {
        Some(f) => f.hello_for(&device.to_string()),
        None => return,
    };
    let Some(msg) = msg else { return };
    match request(ctx, device, &msg).await {
        Ok(Frame {
            from,
            msg: Message::FriendHello { groups, activity },
            ..
        }) => {
            take_hello(ctx, device, &groups, activity, from.as_ref());
        }
        Ok(other) => log::debug!("friends: {} answered {:?}", device.fmt_short(), other.msg),
        Err(e) => log::debug!("friends: {} not reached ({e})", device.fmt_short()),
    }
}

/// Takes in a member's hello (asked or answered). `false`: the device isn't a
/// current member's (nothing was taken but valid group records).
fn take_hello(
    ctx: &Ctx,
    device: EndpointId,
    groups: &[Group],
    activity: Option<Activity>,
    from: Option<&PeerInfo>,
) -> bool {
    let (member, changed, all) = {
        let mut friends = lock(&ctx.shared.friends);
        let Some(f) = friends.as_mut() else {
            return false;
        };
        let changed = f.merge(groups);
        let member = f.member_of(&device.to_string());
        if member.is_some()
            && let Some(peer) = from
            && (peer.relay_url.is_some() || !peer.addrs.is_empty())
        {
            let hint = (peer.relay_url.clone(), peer.addrs.iter().copied().take(16).collect());
            if f.hints.get(&device) != Some(&hint) {
                ctx.emit(Event::FriendDevice {
                    id: device.to_string(),
                    relay_url: hint.0.clone(),
                    addrs: hint.1.clone(),
                });
                f.hints.insert(device, hint);
            }
        }
        (member, changed, f.setup.groups.clone())
    };
    if changed {
        ctx.emit(Event::GroupsChanged(all));
    }
    let Some(member) = member else {
        return false;
    };
    ctx.emit(Event::FriendActivity {
        member,
        activity: activity.map(Activity::bounded),
    });
    true
}

async fn connect(ctx: &Arc<Ctx>, id: EndpointId) -> Result<Connection, String> {
    if let Some(conn) = lock(&ctx.friend_conns).get(&id)
        && conn.close_reason().is_none()
    {
        return Ok(conn.clone());
    }
    let mut addr = EndpointAddr::new(id);
    let hint = lock(&ctx.shared.friends)
        .as_ref()
        .and_then(|f| f.hints.get(&id).cloned());
    let (relay, addrs) = hint.unwrap_or((None, Vec::new()));
    // A friend's device is looked for on its last relay, else on ours (the app's
    // default), and on the LAN (mDNS, by id).
    if let Some(url) = relay
        .or_else(|| ctx.relay_url.clone())
        .and_then(|u| u.parse().ok())
    {
        addr = addr.with_relay_url(url);
    }
    for ip in addrs {
        addr = addr.with_ip_addr(ip);
    }
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, ctx.endpoint.connect(addr, FRIENDS_ALPN))
        .await
        .map_err(|_| "timed out".to_owned())? // i18n-ignore
        .map_err(|e| e.to_string())?;
    lock(&ctx.friend_conns).insert(id, conn.clone());
    tokio::spawn(answer_all(ctx.clone(), conn.clone()));
    Ok(conn)
}

/// One request to a friend's device (a stale cached connection is retried once).
async fn request(ctx: &Arc<Ctx>, id: EndpointId, msg: &Message) -> Result<Frame, String> {
    let mut last = String::new();
    for _ in 0..2 {
        let conn = connect(ctx, id).await?;
        match exchange(&conn, &ctx.me_for_friends(), msg).await {
            Ok(Frame {
                msg: Message::Error { code, .. },
                ..
            }) => return Err(code),
            Ok(frame) => return Ok(frame),
            Err(e) => {
                last = e;
                lock(&ctx.friend_conns).remove(&id);
            }
        }
    }
    Err(last)
}

/// An incoming connection from a friend's device.
pub(super) async fn serve_friend(ctx: Arc<Ctx>, conn: Connection) {
    if lock(&ctx.shared.friends).is_none() {
        conn.close(1u32.into(), b"friends off");
        return;
    }
    lock(&ctx.friend_conns).insert(conn.remote_id(), conn.clone());
    answer_all(ctx, conn).await;
}

async fn answer_all(ctx: Arc<Ctx>, conn: Connection) {
    let id = conn.remote_id();
    while let Ok((send, recv)) = conn.accept_bi().await {
        let ctx = ctx.clone();
        tokio::spawn(async move { answer(&ctx, id, send, recv).await });
    }
}

async fn answer(ctx: &Ctx, id: EndpointId, mut send: SendStream, mut recv: RecvStream) {
    let reply = match tokio::time::timeout(REQUEST_TIMEOUT, read_frame(&mut recv)).await {
        Err(_) | Ok(Err(ReadError::Io(_))) => return,
        Ok(Err(ReadError::Decode(e))) => Message::error(code::UNSUPPORTED, e.to_string()),
        Ok(Ok(frame)) => match frame.msg {
            Message::FriendHello { groups, activity } => {
                if take_hello(ctx, id, &groups, activity, frame.from.as_ref()) {
                    lock(&ctx.shared.friends)
                        .as_ref()
                        .and_then(|f| f.hello_for(&id.to_string()))
                        .unwrap_or_else(|| Message::error(code::NOT_MEMBER, ""))
                } else {
                    Message::error(code::NOT_MEMBER, "")
                }
            }
            Message::ListenTogether { group, link } => {
                let member = lock(&ctx.shared.friends).as_ref().and_then(|f| {
                    let g = f.setup.groups.iter().find(|g| g.id() == group)?;
                    g.is_member(&f.setup.identity.id())
                        .then(|| g.member_of_device(&id.to_string()))
                        .flatten()
                        .map(|m| m.member.clone())
                });
                match member {
                    Some(member) => {
                        ctx.emit(Event::ListenInvite {
                            member,
                            group,
                            link: link.chars().take(2048).collect(),
                        });
                        Message::Ok
                    }
                    None => Message::error(code::NOT_MEMBER, ""),
                }
            }
            other => Message::error(code::UNSUPPORTED, format!("{other:?}")),
        },
    };
    if write_frame(&mut send, Some(&ctx.me_for_friends()), &reply).await.is_ok() {
        let _ = send.finish();
        let _ = send.stopped().await;
    }
}

// ---- invitations ----------------------------------------------------------------------

/// The inviter's side (see `crate::friends`): SPAKE2 with the code, then the group,
/// the newcomer's record, the group with it in.
pub(super) async fn serve_invite(ctx: &Arc<Ctx>, conn: &Connection) {
    let joiner = conn.remote_id();
    let Ok(Ok((mut send, mut recv))) =
        tokio::time::timeout(REQUEST_TIMEOUT, conn.accept_bi()).await
    else {
        return;
    };
    let result = tokio::time::timeout(REQUEST_TIMEOUT * 2, async {
        let Ok(Frame {
            msg: Message::PairStart { spake },
            ..
        }) = read_frame(&mut recv).await
        else {
            return Err(Message::error(code::MALFORMED, ""));
        };
        let (group_id, code) = {
            let mut inviting = lock(&ctx.shared.inviting);
            match inviting.as_mut() {
                Some((group, p))
                    if p.until > Instant::now() && !p.done && p.attempts < MAX_ATTEMPTS =>
                {
                    p.attempts += 1;
                    (group.clone(), p.code.clone())
                }
                _ => return Err(Message::error(code::NOT_PAIRING, "")),
            }
        };
        let (exchange, ours) = Exchange::start(
            Side::Shower,
            &code,
            *ctx.endpoint.id().as_bytes(),
            *joiner.as_bytes(),
        );
        let keys = exchange
            .finish(&spake)
            .ok_or_else(|| Message::error(code::MALFORMED, ""))?;
        let challenge = Message::PairChallenge {
            spake: ours,
            confirm: keys.confirm(),
        };
        write_frame(&mut send, None, &challenge)
            .await
            .map_err(|_| Message::Ok)?;
        let Ok(Frame {
            msg: Message::PairConfirm { confirm, peer },
            ..
        }) = read_frame(&mut recv).await
        else {
            return Err(Message::Ok);
        };
        if !keys.verify(&confirm) {
            return Err(Message::error(code::WRONG_CODE, ""));
        }
        // The code is proven: the group goes to the newcomer.
        let (group, identity) = {
            let friends = lock(&ctx.shared.friends);
            let f = friends.as_ref().ok_or(Message::error(code::NOT_PAIRING, ""))?;
            let g = f.setup.groups.iter().find(|g| g.id() == group_id);
            (
                g.cloned().ok_or(Message::error(code::NOT_PAIRING, ""))?,
                f.setup.identity.clone(),
            )
        };
        let msg = Message::InviteGroup {
            group: group.shareable(),
        };
        write_frame(&mut send, None, &msg)
            .await
            .map_err(|_| Message::Ok)?;
        let Ok(Frame {
            msg: Message::InviteJoin { record },
            ..
        }) = read_frame(&mut recv).await
        else {
            return Err(Message::Ok);
        };
        let joined = admit(&group, &identity, &record, &joiner.to_string())
            .ok_or_else(|| Message::error(code::NOT_MEMBER, ""))?;
        Ok((joined, record.name, peer))
    })
    .await
    .unwrap_or(Err(Message::Ok));

    match result {
        Ok((group, name, peer)) => {
            let all = {
                let mut friends = lock(&ctx.shared.friends);
                let Some(f) = friends.as_mut() else { return };
                if let Some(mine) = f.setup.groups.iter_mut().find(|g| g.id() == group.id()) {
                    mine.merge(&group);
                }
                if peer.relay_url.is_some() || !peer.addrs.is_empty() {
                    f.hints
                        .insert(joiner, (peer.relay_url.clone(), peer.addrs.clone()));
                }
                f.setup.groups.clone()
            };
            *lock(&ctx.shared.inviting) = None;
            ctx.endpoint.set_user_data_for_address_lookup(None);
            if write_frame(&mut send, None, &Message::InviteDone { group: group.shareable() })
                .await
                .is_ok()
            {
                let _ = send.finish();
                let _ = send.stopped().await;
            }
            ctx.emit(Event::GroupsChanged(all));
            ctx.emit(Event::InviteEnded { joined: Some(name) });
            // The others learn of the newcomer.
            hello_all(ctx, Some(group.id()));
        }
        Err(reply) => {
            let too_many = lock(&ctx.shared.inviting)
                .as_ref()
                .is_some_and(|(_, p)| p.attempts >= MAX_ATTEMPTS);
            if reply != Message::Ok {
                let reply = match reply {
                    Message::Error { code, .. } if code == code::WRONG_CODE && too_many => {
                        Message::error(code::TOO_MANY_TRIES, "")
                    }
                    other => other,
                };
                let _ = write_frame(&mut send, None, &reply).await;
                let _ = send.finish();
                let _ = send.stopped().await;
            }
            if too_many {
                *lock(&ctx.shared.inviting) = None;
                ctx.endpoint.set_user_data_for_address_lookup(None);
                ctx.emit(Event::InviteEnded { joined: None });
            }
        }
    }
}

/// The group with a newcomer in: vouched for by `inviter`, with its own record (which
/// must be its own and list the device it joins from). `None`: refused.
pub(crate) fn admit(
    group: &Group,
    inviter: &Identity,
    record: &MemberRecord,
    device: &str,
) -> Option<Group> {
    if !record.devices.iter().any(|d| d == device) {
        return None;
    }
    let mut joined = group.clone();
    joined
        .vouch(inviter, &record.member, now_unix_ms())
        .ok()?;
    let theirs = Group {
        header: group.header.clone(),
        members: vec![record.clone()],
        removals: Vec::new(),
    };
    joined.merge(&theirs);
    (joined.member_of_device(device)?.member == record.member).then_some(joined)
}

/// The newcomer's side: finds the inviter (link, or a device showing an invitation on
/// the LAN) and joins.
async fn join(ctx: &Arc<Ctx>, input: PairInput) -> Result<Group, PairError> {
    let code = input.code().to_owned();
    let targets: Vec<EndpointAddr> = match &input {
        PairInput::Link(link) => {
            let id: EndpointId = link
                .id
                .parse()
                .map_err(|_| PairError::Network("bad device id".into()))?; // i18n-ignore
            let mut addr = EndpointAddr::new(id);
            if let Some(url) = link.relay_url.as_deref().and_then(|u| u.parse().ok()) {
                addr = addr.with_relay_url(url);
            }
            for ip in &link.addrs {
                addr = addr.with_ip_addr(*ip);
            }
            vec![addr]
        }
        PairInput::Code(_) => {
            let deadline = Instant::now() + FIND_TIMEOUT;
            loop {
                let found: Vec<EndpointId> =
                    lock(&ctx.shared.inviting_seen).iter().copied().collect();
                if !found.is_empty() {
                    break found.into_iter().map(EndpointAddr::new).collect();
                }
                if Instant::now() >= deadline {
                    return Err(PairError::NotFound);
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    let mut last = PairError::NotFound;
    for target in targets {
        match join_with(ctx, target, &code).await {
            Ok(group) => return Ok(group),
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn join_with(ctx: &Arc<Ctx>, target: EndpointAddr, code: &str) -> Result<Group, PairError> {
    let inviter = target.id;
    if inviter == ctx.endpoint.id() {
        return Err(PairError::NotPairing);
    }
    let (identity, name) = match lock(&ctx.shared.friends).as_ref() {
        Some(f) => (f.setup.identity.clone(), f.setup.name.clone()),
        None => return Err(PairError::NotPairing),
    };
    let net = |e: String| PairError::Network(e);
    let hint = (
        target.relay_urls().next().map(|u| u.to_string()),
        target.ip_addrs().copied().collect::<Vec<_>>(),
    );
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, ctx.endpoint.connect(target, INVITE_ALPN))
        .await
        .map_err(|_| net("timed out".into()))? // i18n-ignore
        .map_err(|e| net(e.to_string()))?;
    let me = ctx.endpoint.id();
    let result = tokio::time::timeout(REQUEST_TIMEOUT * 2, async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| net(e.to_string()))?;
        let (exchange, ours) =
            Exchange::start(Side::Joiner, code, *inviter.as_bytes(), *me.as_bytes());
        write_frame(&mut send, None, &Message::PairStart { spake: ours })
            .await
            .map_err(net)?;
        let read = |e: ReadError| net(e.to_string());
        let keys = match read_frame(&mut recv).await.map_err(read)?.msg {
            Message::PairChallenge { spake, confirm } => {
                let keys = exchange
                    .finish(&spake)
                    .ok_or_else(|| net("bad answer".into()))?; // i18n-ignore
                if !keys.verify(&confirm) {
                    return Err(PairError::WrongCode);
                }
                keys
            }
            Message::Error { code: c, .. } if c == code::NOT_PAIRING => {
                return Err(PairError::NotPairing);
            }
            other => return Err(net(format!("unexpected answer {other:?}"))), // i18n-ignore
        };
        let confirm = Message::PairConfirm {
            confirm: keys.confirm(),
            peer: ctx.me_for_friends(),
        };
        write_frame(&mut send, None, &confirm).await.map_err(net)?;
        let group = match read_frame(&mut recv).await.map_err(read)?.msg {
            Message::InviteGroup { group } if group.valid() => group,
            Message::Error { code: c, .. } if c == code::WRONG_CODE => {
                return Err(PairError::WrongCode);
            }
            Message::Error { code: c, .. }
                if c == code::TOO_MANY_TRIES || c == code::NOT_PAIRING =>
            {
                return Err(PairError::NotPairing);
            }
            other => return Err(net(format!("unexpected answer {other:?}"))), // i18n-ignore
        };
        // This member's own record: as it was (a restored identity keeps its devices),
        // with this device added.
        let mut mine = group.clone();
        let my_id = identity.id();
        if mine.record(&my_id).is_none() {
            mine.members.push(MemberRecord {
                member: my_id.clone(),
                ..MemberRecord::default()
            });
        }
        mine.set_self(&identity, &name, Some(&me.to_string()), false, now_unix_ms());
        let record = mine.record(&my_id).cloned().unwrap_or_default();
        write_frame(&mut send, None, &Message::InviteJoin { record })
            .await
            .map_err(net)?;
        let _ = send.finish();
        match read_frame(&mut recv).await.map_err(read)?.msg {
            Message::InviteDone { group } if group.is_member(&my_id) => Ok(group),
            Message::Error { code: c, .. } => Err(net(c)),
            other => Err(net(format!("unexpected answer {other:?}"))), // i18n-ignore
        }
    })
    .await
    .map_err(|_| net("timed out".into()))?; // i18n-ignore
    conn.close(0u32.into(), b"done");
    let group = result?;
    let all = {
        let mut friends = lock(&ctx.shared.friends);
        let Some(f) = friends.as_mut() else {
            return Err(PairError::NotPairing);
        };
        match f.setup.groups.iter_mut().find(|g| g.id() == group.id()) {
            Some(known) => {
                known.merge(&group);
            }
            None => f.setup.groups.push(group.clone()),
        }
        if hint.0.is_some() || !hint.1.is_empty() {
            f.hints.insert(inviter, hint);
        }
        f.setup.groups.clone()
    };
    ctx.emit(Event::GroupsChanged(all));
    // Say hello to the others (they learn this device; this one learns what they play).
    ctx.command(Command::Friends(FriendsCommand::HelloAll));
    Ok(group)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitting_checks_the_record() {
        let (ann, bob, eve) = (Identity::generate(), Identity::generate(), Identity::generate());
        let group = Group::create(&ann, "Friday", "Ann", "dev-ann", 1);
        let record = |who: &Identity, device: &str| {
            let mut g = group.clone();
            g.members.push(MemberRecord {
                member: who.id(),
                ..MemberRecord::default()
            });
            g.set_self(who, "Bob", Some(device), false, 5);
            g.record(&who.id()).cloned().unwrap()
        };
        let joined = admit(&group, &ann, &record(&bob, "dev-bob"), "dev-bob").unwrap();
        assert!(joined.is_member(&bob.id()) && joined.valid());
        // From another device than the record names: refused.
        assert!(admit(&group, &ann, &record(&bob, "dev-bob"), "dev-x").is_none());
        // A record signed by someone else than the member it names: refused.
        let mut forged = record(&bob, "dev-bob");
        forged.member = eve.id();
        assert!(admit(&group, &ann, &forged, "dev-bob").is_none());
        // An inviter who isn't a member can't vouch.
        assert!(admit(&group, &eve, &record(&bob, "dev-bob"), "dev-bob").is_none());
    }
}
