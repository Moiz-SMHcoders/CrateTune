//! A member identity: an Ed25519 key per installation (kept in the keychain), whose
//! public half is the member id friends know you by. It signs what you say about
//! yourself in a group (name, devices, leaving) and the invitations you hand out.
//!
//! The **recovery phrase** moves the identity to another computer: the 32-byte key
//! and a 2-byte checksum (SHA-256) in Crockford base32, 55 characters shown in 11
//! groups of 5 (`7KQ2M-…`). Typed back in (any case; dashes, spaces and the letters
//! I, L, O read as 1, 1, 0), it gives the same member id.

use base64::Engine as _;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use sha2::{Digest as _, Sha256};

/// Crockford's base32 (no I, L, O, U).
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const PHRASE_CHARS: usize = 55;
const GROUP: usize = 5;

#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    seed: [u8; 32],
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    /// Not 55 characters of base32.
    Malformed,
    /// A character is wrong (the checksum doesn't match).
    Checksum,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl Identity {
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&crate::spotify::random_bytes(32));
        Self { seed }
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { seed }
    }

    fn pair(&self) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&self.seed).expect("any 32 bytes are a seed")
    }

    /// The member id: the public key, hex.
    pub fn id(&self) -> String {
        hex(self.pair().public_key().as_ref())
    }

    /// A signature over `msg` (base64url).
    pub fn sign(&self, msg: &[u8]) -> String {
        b64(self.pair().sign(msg).as_ref())
    }

    /// How the key is stored (hex; a secret, never logged).
    pub fn to_secret(&self) -> String {
        hex(&self.seed)
    }

    pub fn from_secret(stored: &str) -> Option<Self> {
        let bytes = unhex(stored)?;
        Some(Self {
            seed: bytes.try_into().ok()?,
        })
    }

    pub fn recovery_phrase(&self) -> String {
        let mut bytes = self.seed.to_vec();
        bytes.extend_from_slice(&Sha256::digest(self.seed)[..2]);
        let mut chars = String::with_capacity(PHRASE_CHARS + PHRASE_CHARS / GROUP);
        let (mut acc, mut bits) = (0u32, 0u32);
        let mut n = 0;
        let mut push = |v: u32, chars: &mut String| {
            if n > 0 && n % GROUP == 0 {
                chars.push('-');
            }
            chars.push(ALPHABET[v as usize] as char);
            n += 1;
        };
        for b in bytes {
            acc = (acc << 8) | u32::from(b);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                push((acc >> bits) & 31, &mut chars);
            }
        }
        if bits > 0 {
            push((acc << (5 - bits)) & 31, &mut chars);
        }
        chars
    }

    pub fn from_recovery_phrase(phrase: &str) -> Result<Self, PhraseError> {
        let mut values = Vec::with_capacity(PHRASE_CHARS);
        for c in phrase.chars() {
            let c = match c.to_ascii_uppercase() {
                ' ' | '-' | '\n' | '\t' | '.' => continue,
                'I' | 'L' => '1',
                'O' => '0',
                c => c,
            };
            let v = ALPHABET
                .iter()
                .position(|a| c.is_ascii() && *a == c as u8)
                .ok_or(PhraseError::Malformed)?;
            values.push(v as u32);
        }
        if values.len() != PHRASE_CHARS {
            return Err(PhraseError::Malformed);
        }
        let mut bytes = Vec::with_capacity(34);
        let (mut acc, mut bits) = (0u32, 0u32);
        for v in values {
            acc = (acc << 5) | v;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                bytes.push((acc >> bits) as u8);
            }
        }
        // 275 bits: 34 bytes and 3 padding bits, which must be zero.
        if bytes.len() != 34 || acc & ((1 << bits) - 1) != 0 {
            return Err(PhraseError::Checksum);
        }
        let seed: [u8; 32] = bytes[..32].try_into().map_err(|_| PhraseError::Malformed)?;
        if Sha256::digest(seed)[..2] != bytes[32..] {
            return Err(PhraseError::Checksum);
        }
        Ok(Self { seed })
    }
}

/// Whether `sig` (base64url) is `member`'s (hex public key) signature over `msg`.
pub fn verify(member: &str, msg: &[u8], sig: &str) -> bool {
    let (Some(key), Ok(sig)) = (
        unhex(member).filter(|k| k.len() == 32),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig),
    ) else {
        return false;
    };
    UnparsedPublicKey::new(&ED25519, key)
        .verify(msg, &sig)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sign_and_round_trip() {
        let me = Identity::generate();
        assert_eq!(me.id().len(), 64);
        assert_ne!(me.id(), Identity::generate().id());
        let sig = me.sign(b"hello");
        assert!(verify(&me.id(), b"hello", &sig));
        assert!(!verify(&me.id(), b"hellO", &sig));
        assert!(!verify(&Identity::generate().id(), b"hello", &sig));
        assert!(!verify("zz", b"hello", &sig));
        assert!(!verify(&me.id(), b"hello", "not base64!"));
        let stored = me.to_secret();
        assert_eq!(Identity::from_secret(&stored), Some(me.clone()));
        assert_eq!(Identity::from_secret("abc"), None);
        assert!(
            !format!("{me:?}").contains(&stored),
            "the key isn't in Debug"
        );
    }

    #[test]
    fn recovery_phrases() {
        for _ in 0..20 {
            let me = Identity::generate();
            let phrase = me.recovery_phrase();
            assert_eq!(phrase.len(), 55 + 10, "{phrase}");
            assert_eq!(phrase.split('-').count(), 11);
            assert_eq!(Identity::from_recovery_phrase(&phrase), Ok(me.clone()));
            // Any case, spaces instead of dashes, confusable letters.
            let typed = phrase.to_lowercase().replace('-', " ").replace('1', "l");
            assert_eq!(Identity::from_recovery_phrase(&typed), Ok(me.clone()));
            // One character changed: caught by the checksum.
            let mut wrong: Vec<char> = phrase.chars().collect();
            wrong[3] = if wrong[3] == 'A' { 'B' } else { 'A' };
            let wrong: String = wrong.into_iter().collect();
            assert!(Identity::from_recovery_phrase(&wrong).is_err());
        }
        assert_eq!(
            Identity::from_recovery_phrase("ABC"),
            Err(PhraseError::Malformed)
        );
        assert_eq!(
            Identity::from_recovery_phrase(&"U".repeat(55)),
            Err(PhraseError::Malformed)
        );
    }
}
