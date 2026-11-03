//! A friend group: who is in it, as signed records every member keeps a copy of and
//! passes on (no server holds the list). Copies merge like the sync document's
//! registers: per record, the newest valid one wins.
//!
//! * The **header** (id, name, owner) is signed by the owner (who made the group);
//!   only the owner renames it.
//! * A **member record** has two signed parts. What the member says about itself
//!   (name, its devices' ids, "I left") is signed by the member, newest wins. Who let
//!   it in (`added_by`, when) is signed by that inviter; a member counts only if the
//!   chain of inviters leads back to the owner, so nobody can add themselves.
//! * A **removal** is signed by the owner and permanent: the member's own newer
//!   records don't bring it back.
//!
//! Every signature covers the group id, so a record can't be replayed into another
//! group; a copy with a bad signature is dropped as a whole record.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::identity::{Identity, verify};

/// Members a group can have (activity goes to each one's devices).
pub const MAX_MEMBERS: usize = 32;
/// Devices kept per member (newest last).
pub const MAX_DEVICES: usize = 8;
/// Longest group / member name.
pub const MAX_NAME: usize = 40;

/// What is signed: tagged, length-prefixed parts (no ambiguity between fields).
fn signed(tag: &str, parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + parts.iter().map(|p| p.len() + 4).sum::<usize>());
    out.extend_from_slice(b"cratetune-friends/");
    out.extend_from_slice(tag.as_bytes());
    for part in parts {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

/// A name as stored: trimmed, no control characters, at most [`MAX_NAME`] characters.
pub fn clean_name(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupHeader {
    /// Random, 32 hex characters.
    pub id: String,
    pub name: String,
    /// The member id of the group's maker.
    pub owner: String,
    pub created_ms: u64,
    pub stamp: u64,
    pub sig: String,
}

impl GroupHeader {
    fn bytes(&self) -> Vec<u8> {
        signed(
            "header",
            &[
                &self.id,
                &self.name,
                &self.owner,
                &self.created_ms.to_string(),
                &self.stamp.to_string(),
            ],
        )
    }

    fn valid(&self) -> bool {
        verify(&self.owner, &self.bytes(), &self.sig)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MemberRecord {
    pub member: String,
    pub name: String,
    /// The member's devices (endpoint ids); only these speak for it.
    pub devices: Vec<String>,
    pub left: bool,
    pub stamp: u64,
    /// The member's signature over the above.
    pub sig: String,
    /// Who let it in, when, and their signature.
    pub added_by: String,
    pub added_ms: u64,
    pub added_sig: String,
}

impl MemberRecord {
    fn self_bytes(&self, group: &str) -> Vec<u8> {
        let mut parts: Vec<&str> = vec![group, &self.member, &self.name];
        let stamp = self.stamp.to_string();
        let left = if self.left { "1" } else { "0" };
        parts.push(left);
        parts.push(&stamp);
        parts.extend(self.devices.iter().map(String::as_str));
        signed("member", &parts)
    }

    fn vouch_bytes(&self, group: &str) -> Vec<u8> {
        signed(
            "added",
            &[
                group,
                &self.member,
                &self.added_by,
                &self.added_ms.to_string(),
            ],
        )
    }

    fn self_valid(&self, group: &str) -> bool {
        verify(&self.member, &self.self_bytes(group), &self.sig)
    }

    fn vouch_valid(&self, group: &str) -> bool {
        verify(&self.added_by, &self.vouch_bytes(group), &self.added_sig)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Removal {
    pub member: String,
    pub stamp: u64,
    /// The owner's signature.
    pub sig: String,
}

impl Removal {
    fn bytes(&self, group: &str) -> Vec<u8> {
        signed("removed", &[group, &self.member, &self.stamp.to_string()])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    pub header: GroupHeader,
    pub members: Vec<MemberRecord>,
    pub removals: Vec<Removal>,
}

/// Why a group change was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupError {
    /// Only the owner can do that.
    NotOwner,
    /// Not (or no longer) a member.
    NotMember,
    /// The group is full.
    Full,
}

impl Group {
    /// A new group with `me` as its owner and only member.
    pub fn create(me: &Identity, name: &str, my_name: &str, device: &str, now_ms: u64) -> Self {
        let id: String = crate::spotify::random_bytes(16)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut header = GroupHeader {
            id,
            name: clean_name(name),
            owner: me.id(),
            created_ms: now_ms,
            stamp: now_ms,
            sig: String::new(),
        };
        header.sig = me.sign(&header.bytes());
        let mut group = Self {
            header,
            members: Vec::new(),
            removals: Vec::new(),
        };
        let mut record = MemberRecord {
            member: me.id(),
            added_by: me.id(),
            added_ms: now_ms,
            ..MemberRecord::default()
        };
        record.added_sig = me.sign(&record.vouch_bytes(&group.header.id));
        group.members.push(record);
        group.set_self(me, my_name, Some(device), false, now_ms);
        group
    }

    pub fn id(&self) -> &str {
        &self.header.id
    }

    pub fn name(&self) -> &str {
        &self.header.name
    }

    /// Whether the header and every record carry valid signatures (a copy from the
    /// network is checked before it is merged; see [`Self::merge`]).
    pub fn valid(&self) -> bool {
        let id = self.id();
        self.header.valid()
            && self.members.iter().all(|m| {
                // Half-made records (vouched for, not joined yet; or the reverse) are
                // fine; a part that is there must be right.
                (m.sig.is_empty() || m.self_valid(id))
                    && (m.added_sig.is_empty() || m.vouch_valid(id))
                    && !(m.sig.is_empty() && m.added_sig.is_empty())
            })
            && self
                .removals
                .iter()
                .all(|r| verify(&self.header.owner, &r.bytes(id), &r.sig))
    }

    /// Members let in through a chain of inviters from the owner (removed ones too:
    /// whom they let in stays).
    fn vouched(&self) -> HashSet<&str> {
        let mut ok: HashSet<&str> = HashSet::from([self.header.owner.as_str()]);
        loop {
            let before = ok.len();
            for m in &self.members {
                if ok.contains(m.added_by.as_str()) {
                    ok.insert(m.member.as_str());
                }
            }
            if ok.len() == before {
                return ok;
            }
        }
    }

    /// The current members: vouched for, not left, not removed.
    pub fn current(&self) -> Vec<&MemberRecord> {
        let vouched = self.vouched();
        let removed: HashSet<&str> = self.removals.iter().map(|r| r.member.as_str()).collect();
        self.members
            .iter()
            .filter(|m| {
                vouched.contains(m.member.as_str())
                    && !m.left
                    && !removed.contains(m.member.as_str())
                    && !m.sig.is_empty()
            })
            .collect()
    }

    pub fn is_member(&self, member: &str) -> bool {
        self.current().iter().any(|m| m.member == member)
    }

    /// The current member a device (endpoint id) belongs to.
    pub fn member_of_device(&self, device: &str) -> Option<&MemberRecord> {
        self.current()
            .into_iter()
            .find(|m| m.devices.iter().any(|d| d == device))
    }

    pub fn record(&self, member: &str) -> Option<&MemberRecord> {
        self.members.iter().find(|m| m.member == member)
    }

    /// Writes (and signs) what `me` says about itself: name, a device to add, left.
    pub fn set_self(
        &mut self,
        me: &Identity,
        name: &str,
        device: Option<&str>,
        left: bool,
        now_ms: u64,
    ) {
        let id = self.header.id.clone();
        let my_id = me.id();
        let Some(record) = self.members.iter_mut().find(|m| m.member == my_id) else {
            return;
        };
        record.name = clean_name(name);
        if let Some(device) = device {
            record.devices.retain(|d| d != device);
            record.devices.push(device.to_owned());
            let extra = record.devices.len().saturating_sub(MAX_DEVICES);
            record.devices.drain(..extra);
        }
        record.left = left;
        record.stamp = record.stamp.max(now_ms.saturating_sub(1)) + 1;
        record.sig = me.sign(&record.self_bytes(&id));
    }

    /// `me` (a member) lets `member` in: the inviter's half of its record. The new
    /// member signs its own half when it joins ([`Self::set_self`]).
    pub fn vouch(&mut self, me: &Identity, member: &str, now_ms: u64) -> Result<(), GroupError> {
        if !self.is_member(&me.id()) {
            return Err(GroupError::NotMember);
        }
        if self.removals.iter().any(|r| r.member == member) {
            return Err(GroupError::NotMember);
        }
        if self.record(member).is_none() && self.current().len() >= MAX_MEMBERS {
            return Err(GroupError::Full);
        }
        let id = self.header.id.clone();
        let at = match self.members.iter().position(|m| m.member == member) {
            Some(i) => i,
            None => {
                self.members.push(MemberRecord {
                    member: member.to_owned(),
                    ..MemberRecord::default()
                });
                self.members.len() - 1
            }
        };
        let record = &mut self.members[at];
        if !record.vouch_valid(&id) {
            record.added_by = me.id();
            record.added_ms = now_ms;
            record.added_sig = me.sign(&record.vouch_bytes(&id));
        }
        Ok(())
    }

    /// The owner removes a member for good.
    pub fn remove(&mut self, me: &Identity, member: &str, now_ms: u64) -> Result<(), GroupError> {
        if me.id() != self.header.owner {
            return Err(GroupError::NotOwner);
        }
        if member == self.header.owner {
            return Err(GroupError::NotMember);
        }
        let mut removal = Removal {
            member: member.to_owned(),
            stamp: now_ms,
            sig: String::new(),
        };
        removal.sig = me.sign(&removal.bytes(&self.header.id));
        self.removals.retain(|r| r.member != member);
        self.removals.push(removal);
        Ok(())
    }

    /// The owner renames the group.
    pub fn rename(&mut self, me: &Identity, name: &str, now_ms: u64) -> Result<(), GroupError> {
        if me.id() != self.header.owner {
            return Err(GroupError::NotOwner);
        }
        self.header.name = clean_name(name);
        self.header.stamp = self.header.stamp.max(now_ms.saturating_sub(1)) + 1;
        self.header.sig = me.sign(&self.header.bytes());
        Ok(())
    }

    /// Takes in another copy of the same group (by id and owner): its newer valid
    /// parts win. Returns whether anything changed.
    pub fn merge(&mut self, other: &Group) -> bool {
        if other.header.id != self.header.id || other.header.owner != self.header.owner {
            return false;
        }
        let id = self.header.id.clone();
        let before = self.clone();
        if other.header.stamp > self.header.stamp && other.header.valid() {
            self.header = other.header.clone();
        }
        let mut members: BTreeMap<String, MemberRecord> = self
            .members
            .drain(..)
            .map(|m| (m.member.clone(), m))
            .collect();
        for theirs in &other.members {
            let self_ok = theirs.self_valid(&id);
            let vouch_ok = theirs.vouch_valid(&id);
            if !self_ok && !vouch_ok {
                continue;
            }
            let mine = members
                .entry(theirs.member.clone())
                .or_insert_with(|| MemberRecord {
                    member: theirs.member.clone(),
                    ..MemberRecord::default()
                });
            if self_ok && (theirs.stamp > mine.stamp || mine.sig.is_empty()) {
                mine.name = theirs.name.clone();
                mine.devices = theirs.devices.clone();
                mine.left = theirs.left;
                mine.stamp = theirs.stamp;
                mine.sig = theirs.sig.clone();
            }
            if vouch_ok && (!mine.vouch_valid(&id) || theirs.added_ms < mine.added_ms) {
                mine.added_by = theirs.added_by.clone();
                mine.added_ms = theirs.added_ms;
                mine.added_sig = theirs.added_sig.clone();
            }
        }
        self.members = members.into_values().collect();
        for theirs in &other.removals {
            let known = self.removals.iter().any(|r| r.member == theirs.member);
            if !known && verify(&self.header.owner, &theirs.bytes(&id), &theirs.sig) {
                self.removals.push(theirs.clone());
            }
        }
        *self != before
    }

    /// The copy to give a member: records of non-members (strangers' half-made
    /// records) left out.
    pub fn shareable(&self) -> Group {
        let vouched = self.vouched();
        Group {
            header: self.header.clone(),
            members: self
                .members
                .iter()
                .filter(|m| vouched.contains(m.member.as_str()))
                .cloned()
                .collect(),
            removals: self.removals.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(group: &mut Group, inviter: &Identity, who: &Identity, name: &str, device: &str) {
        group.vouch(inviter, &who.id(), 10).unwrap();
        group.set_self(who, name, Some(device), false, 10);
    }

    #[test]
    fn a_group_grows_through_invitations() {
        let (ann, bob, cat) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let mut g = Group::create(&ann, " Friday  ", "Ann", "dev-ann", 1);
        assert_eq!(g.name(), "Friday");
        assert!(g.valid());
        assert_eq!(g.current().len(), 1);
        joined(&mut g, &ann, &bob, "Bob", "dev-bob");
        // Bob (a member) invites Cat.
        joined(&mut g, &bob, &cat, "Cat", "dev-cat");
        assert!(g.valid());
        assert_eq!(g.current().len(), 3);
        assert_eq!(g.member_of_device("dev-cat").unwrap().name, "Cat");
        assert!(g.member_of_device("dev-x").is_none());
        // Bob leaves: Cat stays (vouched for through him).
        g.set_self(&bob, "Bob", None, true, 20);
        assert!(!g.is_member(&bob.id()) && g.is_member(&cat.id()));
        // A stranger can't vouch.
        let eve = Identity::generate();
        assert_eq!(g.vouch(&eve, &eve.id(), 30), Err(GroupError::NotMember));
    }

    #[test]
    fn copies_merge_and_forgeries_are_dropped() {
        let (ann, bob) = (Identity::generate(), Identity::generate());
        let mut a = Group::create(&ann, "Friday", "Ann", "dev-ann", 1);
        a.vouch(&ann, &bob.id(), 2).unwrap();
        // Bob's copy: he signs his own record.
        let mut b = a.clone();
        b.set_self(&bob, "Bob", Some("dev-bob"), false, 3);
        assert!(a.merge(&b));
        assert!(!a.merge(&b), "idempotent");
        assert_eq!(a.member_of_device("dev-bob").unwrap().name, "Bob");
        // Ann renames, Bob renames himself, apart: both survive the merge either way.
        a.rename(&ann, "Saturday", 5).unwrap();
        b.set_self(&bob, "Robert", None, false, 6);
        let mut x = a.clone();
        x.merge(&b);
        b.merge(&a);
        assert_eq!(x, b);
        assert_eq!(x.name(), "Saturday");
        assert_eq!(x.record(&bob.id()).unwrap().name, "Robert");
        assert_eq!(bob_rename(&bob), Err(GroupError::NotOwner));

        // Eve adds herself with a self-made vouch: not a member anywhere.
        let eve = Identity::generate();
        let mut forged = x.clone();
        forged.members.push(MemberRecord {
            member: eve.id(),
            added_by: ann.id(),
            added_ms: 9,
            added_sig: eve.sign(b"anything"),
            ..MemberRecord::default()
        });
        forged.vouch(&eve, &eve.id(), 9).ok();
        assert!(!x.merge(&forged) || !x.is_member(&eve.id()));
        assert!(!x.is_member(&eve.id()));
        // A tampered name: the record is dropped, the old one kept.
        let mut tampered = x.clone();
        tampered.members.iter_mut().for_each(|m| {
            if m.member == bob.id() {
                m.name = "Mallory".into();
                m.stamp += 100;
            }
        });
        assert!(!tampered.valid());
        x.merge(&tampered);
        assert_eq!(x.record(&bob.id()).unwrap().name, "Robert");
        // Another group's records don't merge in.
        let other = Group::create(&ann, "Other", "Ann", "dev-ann", 1);
        assert!(!x.merge(&other));

        // Removal by the owner is permanent; Bob can't sign himself back.
        x.remove(&ann, &bob.id(), 50).unwrap();
        let mut back = x.clone();
        back.set_self(&bob, "Bob", None, false, 99);
        x.merge(&back);
        assert!(!x.is_member(&bob.id()));
        assert_eq!(x.remove(&bob, &ann.id(), 60), Err(GroupError::NotOwner));
        assert!(x.valid());
        assert_eq!(x.shareable().members.len(), 2);
    }

    fn bob_rename(bob: &Identity) -> Result<(), GroupError> {
        let ann = Identity::generate();
        let mut g = Group::create(&ann, "x", "Ann", "d", 1);
        g.rename(bob, "mine", 2)
    }

    #[test]
    fn devices_and_names_are_bounded() {
        let ann = Identity::generate();
        let mut g = Group::create(&ann, &"n".repeat(100), "Ann\u{7}", "d0", 1);
        assert_eq!(g.name().chars().count(), MAX_NAME);
        assert_eq!(g.record(&ann.id()).unwrap().name, "Ann");
        for i in 1..20 {
            g.set_self(&ann, "Ann", Some(&format!("d{i}")), false, 1 + i);
        }
        let devices = &g.record(&ann.id()).unwrap().devices;
        assert_eq!(devices.len(), MAX_DEVICES);
        assert_eq!(devices.last().unwrap(), "d19");
        assert!(g.valid());
    }
}
