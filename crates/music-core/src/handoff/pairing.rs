//! Pairing two devices with a short code.
//!
//! The device that shows the code (the *shower*) generates 8 random characters of
//! Crockford's base32 (40 bits, shown as `ABCD-EFGH`) and, while the code is shown,
//! answers connections on [`PAIR_ALPN`] and advertises "pairing" on the LAN (mDNS
//! user data). The other device (the *joiner*) is given the code (typed: it then
//! looks for a device that is pairing on the LAN) or the pairing link (which also
//! says where the shower is: its id, relay and addresses; for another network).
//!
//! The code is a SPAKE2 password: both sides run SPAKE2 over the (already encrypted)
//! iroh connection, with the two device ids as the SPAKE2 identities, and confirm
//! the resulting key with an HMAC each way. A wrong guess teaches an attacker nothing
//! about the code (one guess per attempt, no offline search), and the shower stops
//! after [`MAX_ATTEMPTS`] attempts or [`CODE_TTL`]. After it, each side remembers the
//! other's id (its public key): only those ids are accepted on [`DEVICES_ALPN`].
//!
//! [`DEVICES_ALPN`]: super::messages::DEVICES_ALPN

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use spake2::{Ed25519Group, Identity, Password, Spake2};

#[cfg(doc)]
use super::messages::PAIR_ALPN;

/// Characters of a code.
pub const CODE_LEN: usize = 8;
/// A shown code stops working after this long.
pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// Pairing attempts (right or wrong) one code allows.
pub const MAX_ATTEMPTS: u32 = 5;
/// The mDNS user data of a device that is showing a code.
pub const PAIRING_USER_DATA: &str = "pairing";
/// Scheme of the pairing link.
pub const LINK_SCHEME: &str = "music-devices://pair";

/// Crockford's base32 (no I, L, O, U).
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A new random code (normalized: 8 characters, no dash).
pub fn new_code() -> String {
    crate::spotify::random_bytes(CODE_LEN)
        .into_iter()
        .map(|b| ALPHABET[usize::from(b & 31)] as char)
        .collect()
}

/// `ABCDEFGH` → `ABCD-EFGH`.
pub fn display_code(code: &str) -> String {
    if code.len() == CODE_LEN && code.is_ascii() {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_owned()
    }
}

/// What the user typed → the code: any case, spaces and dashes ignored, and the
/// letters people confuse with digits (I, L → 1; O → 0) mapped as Crockford does.
pub fn normalize_code(input: &str) -> Option<String> {
    let mut code = String::with_capacity(CODE_LEN);
    for c in input.chars() {
        let c = match c.to_ascii_uppercase() {
            ' ' | '-' | '_' | '.' => continue,
            'I' | 'L' => '1',
            'O' => '0',
            c if c.is_ascii() && ALPHABET.contains(&(c as u8)) => c,
            _ => return None,
        };
        code.push(c);
    }
    (code.len() == CODE_LEN).then_some(code)
}

/// Everything the joiner needs to find the shower from another network: the link
/// shown (and as a QR code) next to the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingLink {
    /// The shower's device id (hex public key).
    pub id: String,
    pub code: String,
    pub relay_url: Option<String>,
    pub addrs: Vec<SocketAddr>,
}

impl PairingLink {
    /// `music-devices://pair?id=…&code=…&relay=…&addr=…&addr=…`.
    pub fn to_link(&self) -> String {
        let mut url = url::Url::parse(LINK_SCHEME).expect("valid scheme");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("id", &self.id);
            q.append_pair("code", &self.code);
            if let Some(relay) = &self.relay_url {
                q.append_pair("relay", relay);
            }
            for addr in &self.addrs {
                q.append_pair("addr", &addr.to_string());
            }
        }
        url.to_string()
    }

    pub fn parse(link: &str) -> Option<Self> {
        let url = url::Url::parse(link.trim()).ok()?;
        if url.scheme() != "music-devices" || url.host_str() != Some("pair") {
            return None;
        }
        let mut link = PairingLink {
            id: String::new(),
            code: String::new(),
            relay_url: None,
            addrs: Vec::new(),
        };
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "id" => link.id = value.into_owned(),
                "code" => link.code = normalize_code(&value)?,
                "relay" => link.relay_url = Some(value.into_owned()),
                "addr" => link.addrs.extend(value.parse::<SocketAddr>().ok()),
                _ => {}
            }
        }
        (!link.id.is_empty() && !link.code.is_empty()).then_some(link)
    }
}

/// What was entered in "Pair with a code".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairInput {
    /// Just the code: look for the device on this network.
    Code(String),
    Link(PairingLink),
}

impl PairInput {
    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        if input.starts_with("music-devices:") {
            PairingLink::parse(input).map(Self::Link)
        } else {
            normalize_code(input).map(Self::Code)
        }
    }

    pub fn code(&self) -> &str {
        match self {
            Self::Code(code) => code,
            Self::Link(link) => &link.code,
        }
    }
}

/// Which end of the exchange this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Shows the code (SPAKE2's "A").
    Shower,
    /// Typed the code (SPAKE2's "B").
    Joiner,
}

/// One side's SPAKE2 run. The identities are the two device ids (shower first),
/// so a key agreed with one device can't be replayed towards another.
pub struct Exchange {
    side: Side,
    spake: Spake2<Ed25519Group>,
    shower: [u8; 32],
    joiner: [u8; 32],
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .ok()
}

impl Exchange {
    /// Starts SPAKE2; returns the message to send (base64url).
    pub fn start(side: Side, code: &str, shower: [u8; 32], joiner: [u8; 32]) -> (Self, String) {
        let password = Password::new(code.as_bytes());
        let (a, b) = (Identity::new(&shower), Identity::new(&joiner));
        let (spake, msg) = match side {
            Side::Shower => Spake2::<Ed25519Group>::start_a(&password, &a, &b),
            Side::Joiner => Spake2::<Ed25519Group>::start_b(&password, &a, &b),
        };
        let exchange = Self {
            side,
            spake,
            shower,
            joiner,
        };
        (exchange, b64(&msg))
    }

    /// Finishes with the other side's message: the agreed key (which only matches
    /// the other side's if both used the same code), or `None` for a broken message.
    pub fn finish(self, theirs: &str) -> Option<Keys> {
        let key = self.spake.finish(&unb64(theirs)?).ok()?;
        Some(Keys {
            side: self.side,
            key,
            shower: self.shower,
            joiner: self.joiner,
        })
    }
}

/// The agreed key: key confirmation both ways.
pub struct Keys {
    side: Side,
    key: Vec<u8>,
    shower: [u8; 32],
    joiner: [u8; 32],
}

impl Keys {
    fn tag(&self, of: Side) -> ring::hmac::Tag {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.key);
        let mut ctx = ring::hmac::Context::with_key(&key);
        ctx.update(match of {
            Side::Shower => b"cratetune-pair-confirm-shower",
            Side::Joiner => b"cratetune-pair-confirm-joiner",
        });
        ctx.update(&self.shower);
        ctx.update(&self.joiner);
        ctx.sign()
    }

    /// This side's confirmation (base64url).
    pub fn confirm(&self) -> String {
        b64(self.tag(self.side).as_ref())
    }

    /// Whether the other side's confirmation proves it used the same code
    /// (constant-time comparison).
    pub fn verify(&self, theirs: &str) -> bool {
        let other = match self.side {
            Side::Shower => Side::Joiner,
            Side::Joiner => Side::Shower,
        };
        let Some(theirs) = unb64(theirs) else {
            return false;
        };
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.key);
        let mut msg = Vec::with_capacity(96);
        msg.extend_from_slice(match other {
            Side::Shower => b"cratetune-pair-confirm-shower",
            Side::Joiner => b"cratetune-pair-confirm-joiner",
        });
        msg.extend_from_slice(&self.shower);
        msg.extend_from_slice(&self.joiner);
        ring::hmac::verify(&key, &msg, &theirs).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_look_right_and_normalize() {
        for _ in 0..50 {
            let code = new_code();
            assert_eq!(code.len(), CODE_LEN);
            assert_eq!(normalize_code(&code).as_deref(), Some(code.as_str()));
            assert_eq!(
                normalize_code(&display_code(&code).to_lowercase()).as_deref(),
                Some(code.as_str())
            );
        }
        assert_ne!(new_code(), new_code());
        assert_eq!(display_code("ABCD2345"), "ABCD-2345");
        // Confusable letters, spaces, dashes.
        assert_eq!(normalize_code(" abcd-oil5 ").as_deref(), Some("ABCD0115"));
        assert_eq!(normalize_code("ABCD 2345").as_deref(), Some("ABCD2345"));
        // Wrong length, U (not in the alphabet), other characters.
        assert_eq!(normalize_code("ABC"), None);
        assert_eq!(normalize_code("ABCD-2345-6"), None);
        assert_eq!(normalize_code("ABCDU345"), None);
        assert_eq!(normalize_code("ABCD#345"), None);
        assert_eq!(normalize_code("ÄBCD2345"), None);
    }

    #[test]
    fn pairing_links_round_trip() {
        let link = PairingLink {
            id: "ab".repeat(32),
            code: "ABCD2345".into(),
            relay_url: Some("https://relay.example.com/".into()),
            addrs: vec![
                "192.168.1.5:51234".parse().unwrap(),
                "[fe80::1]:51234".parse().unwrap(),
            ],
        };
        let text = link.to_link();
        assert!(text.starts_with("music-devices://pair?id=abab"), "{text}");
        assert_eq!(PairingLink::parse(&text), Some(link.clone()));
        assert_eq!(PairInput::parse(&text), Some(PairInput::Link(link.clone())));
        assert_eq!(PairInput::parse(&text).unwrap().code(), "ABCD2345");
        assert_eq!(
            PairInput::parse("abcd-2345"),
            Some(PairInput::Code("ABCD2345".into()))
        );
        assert_eq!(PairInput::parse("music-devices://pair?code=ABCD2345"), None);
        assert_eq!(
            PairInput::parse("https://example.com/?id=x&code=ABCD2345"),
            None
        );
        assert_eq!(PairInput::parse("hello"), None);
        assert_eq!(PairInput::parse("hello, world!"), None);
        // Without a relay or addresses (the LAN, by id).
        let bare = PairingLink {
            relay_url: None,
            addrs: Vec::new(),
            ..link
        };
        assert_eq!(PairingLink::parse(&bare.to_link()), Some(bare));
    }

    fn run(shower_code: &str, joiner_code: &str) -> (bool, bool) {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        let (shower, to_joiner) = Exchange::start(Side::Shower, shower_code, a, b);
        let (joiner, to_shower) = Exchange::start(Side::Joiner, joiner_code, a, b);
        let shower = shower.finish(&to_shower).unwrap();
        let joiner = joiner.finish(&to_joiner).unwrap();
        (
            joiner.verify(&shower.confirm()),
            shower.verify(&joiner.confirm()),
        )
    }

    #[test]
    fn spake2_agrees_only_on_the_same_code() {
        assert_eq!(run("ABCD2345", "ABCD2345"), (true, true));
        assert_eq!(run("ABCD2345", "ABCD2346"), (false, false));
        // A side's own confirmation doesn't verify as the other's (no reflection).
        let (a, b) = ([1u8; 32], [2u8; 32]);
        let (shower, m1) = Exchange::start(Side::Shower, "ABCD2345", a, b);
        let (joiner, m2) = Exchange::start(Side::Joiner, "ABCD2345", a, b);
        let shower = shower.finish(&m2).unwrap();
        let _ = joiner.finish(&m1).unwrap();
        assert!(!shower.verify(&shower.confirm()));
        assert!(!shower.verify("not base64 !"));
        // Other identities (another device): no agreement.
        let (shower, m1) = Exchange::start(Side::Shower, "ABCD2345", a, b);
        let (joiner, m2) = Exchange::start(Side::Joiner, "ABCD2345", a, [3u8; 32]);
        let shower = shower.finish(&m2).unwrap();
        let joiner = joiner.finish(&m1).unwrap();
        assert!(!joiner.verify(&shower.confirm()));
        // A broken message.
        let (shower, _) = Exchange::start(Side::Shower, "ABCD2345", a, b);
        assert!(shower.finish("AAAA").is_none());
    }
}
