//! Player fixes: a small signed manifest (`packaging/player-fixes.json`) with
//! the data the stream resolver needs when YouTube changes something, fetched
//! from the app's own repository so playback recovers without an app update.
//!
//! * **Data only.** Client identities (versions, user agents, devices), the
//!   client order, clients to skip, known `signatureTimestamp`s, player scripts
//!   known to break the solver, and a known-good player to pin. Nothing in it is
//!   executed; the only JavaScript the app runs is YouTube's own player script,
//!   in the QuickJS sandbox ([`super::jsc`]). Every field is validated; unknown
//!   fields and clients are ignored.
//! * **Signed** with Ed25519 in [minisign]'s format (prehashed, `ED`): the
//!   manifest is used only if `player-fixes.json.minisig` verifies against
//!   [`PUBLIC_KEY`], compiled into the app. The private key never enters the
//!   repository (see `docs/RELEASING.md` and `scripts/sign-player-fixes.sh`).
//!   Stock `minisign -Vm player-fixes.json -P <key>` verifies it too.
//! * **No rollback.** A manifest replaces the current one only with a higher
//!   `serial`; the copy built into the app is the floor.
//! * **Cached** (`<cache>/player-fixes.json` + `.minisig`, verified again when
//!   read) and refreshed at most every [`CHECK_EVERY`], or sooner (rate-limited)
//!   when every route fails or a player script can't be solved.
//!
//! [minisign]: https://jedisct1.github.io/minisign/

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::WebClient;
use super::client::{ClientOverrides, OVERRIDABLE};
use crate::http::{agent, read_body};
use crate::{Error, Result};

/// The key player-fixes manifests are signed with (minisign public key; the
/// secret half is kept outside the repository).
/// Key id 170691615846FB34.
pub const PUBLIC_KEY: &str = "RWQ0+0ZYYZEGFyPGOq1wauD8dVqHyiGZXtzyBy3GWx66P32t7IwiNrPK";
/// The manifest built into this version (`packaging/player-fixes.json`): the
/// floor for fetched ones.
const BUNDLED: &str = include_str!("../../../../packaging/player-fixes.json");
/// Fetched manifests are checked this often.
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// The most a manifest or signature may weigh.
const MAX_BYTES: usize = 64 * 1024;
const FILE: &str = "player-fixes.json";
const SIG_SUFFIX: &str = ".minisig";
/// Touched after every completed check (its mtime is the last check).
const CHECKED: &str = "player-fixes.checked";

/// What a manifest says, validated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerFixes {
    /// Increases with every published manifest.
    pub serial: u64,
    /// When it was issued (free text, for display).
    pub issued: String,
    pub(crate) clients: ClientOverrides,
    /// Preferred client order (clients not listed keep their order, after these).
    pub(crate) order: Vec<WebClient>,
    /// Clients known not to work now: skipped unless they are the only ones enabled.
    pub(crate) disabled: HashSet<WebClient>,
    pub(crate) players: PlayerHints,
}

/// Hints about YouTube's player scripts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlayerHints {
    /// Known `signatureTimestamp` per player id (when the script's can't be read).
    pub sts: HashMap<String, u32>,
    /// Player ids whose challenges the solver can't solve.
    pub broken: HashSet<String>,
    /// A known-good player to use instead of a broken (or any, with `force`) one.
    pub pin: Option<PinnedPlayer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinnedPlayer {
    pub id: String,
    pub sts: u32,
    /// Use it even when the watch page's player isn't known to be broken.
    pub force: bool,
}

impl PlayerHints {
    /// The player (id, known sts) to use when the watch page names `player_id`.
    pub(crate) fn choose(&self, player_id: &str) -> (String, Option<u32>) {
        if let Some(pin) = &self.pin
            && (pin.force || self.broken.contains(player_id))
            && pin.id != player_id
        {
            return (pin.id.clone(), Some(pin.sts));
        }
        (player_id.to_owned(), self.sts.get(player_id).copied())
    }
}

impl PlayerFixes {
    /// The manifest built into the app.
    pub fn bundled() -> Self {
        parse(BUNDLED.as_bytes()).unwrap_or_else(|e| {
            log::error!("youtube: the built-in player fixes are invalid: {e}");
            Self::default()
        })
    }

    /// The built-in manifest's serial (fetched ones must be above it).
    pub fn bundled_serial() -> u64 {
        static SERIAL: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *SERIAL.get_or_init(|| Self::bundled().serial)
    }

    /// `enabled` clients, reordered by the manifest's preference, without the
    /// disabled ones (unless that would leave none).
    pub(crate) fn arrange(&self, enabled: &[WebClient]) -> Vec<WebClient> {
        let mut out: Vec<WebClient> = enabled.to_vec();
        // Stable: unlisted clients keep their relative order after the listed ones.
        out.sort_by_key(|c| self.order.iter().position(|o| o == c).unwrap_or(usize::MAX));
        let usable: Vec<WebClient> = out
            .iter()
            .copied()
            .filter(|c| !self.disabled.contains(c))
            .collect();
        if usable.is_empty() { out } else { usable }
    }
}

/// Parses and validates a manifest (not its signature: see [`verify`]).
pub fn parse(bytes: &[u8]) -> Result<PlayerFixes> {
    let bad = |what: &str| Error::Parse(format!("player fixes: {what}"));
    let v: Value = serde_json::from_slice(bytes).map_err(|e| bad(&format!("not JSON ({e})")))?;
    if v["schema"].as_u64() != Some(1) {
        return Err(bad("unknown schema"));
    }
    let serial = v["serial"].as_u64().ok_or_else(|| bad("no serial"))?;
    let issued = v["issued"]
        .as_str()
        .filter(|s| plain(s, 40))
        .unwrap_or_default()
        .to_owned();

    let mut clients = ClientOverrides::default();
    for (key, fields) in v["clients"].as_object().into_iter().flatten() {
        let Some(client) = WebClient::from_key(key) else {
            continue;
        };
        let mut set = HashMap::new();
        for (field, value) in fields.as_object().into_iter().flatten() {
            if !OVERRIDABLE.contains(&field.as_str()) {
                continue;
            }
            let value = value
                .as_str()
                .filter(|s| !s.is_empty() && plain(s, 300))
                .ok_or_else(|| bad(&format!("{key}.{field}")))?;
            set.insert(field.clone(), value.to_owned());
        }
        if !set.is_empty() {
            clients.0.insert(client, set);
        }
    }
    let client_list = |name: &str| -> Vec<WebClient> {
        v[name]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str().and_then(WebClient::from_key))
            .collect()
    };
    let order = client_list("order");
    let disabled = client_list("disabled").into_iter().collect();

    let p = &v["players"];
    let mut players = PlayerHints::default();
    for (id, sts) in p["sts"].as_object().into_iter().flatten() {
        let sts = sts.as_u64().and_then(valid_sts);
        match (player_id(id), sts) {
            (true, Some(sts)) => {
                players.sts.insert(id.clone(), sts);
            }
            _ => return Err(bad(&format!("players.sts.{id}"))),
        }
    }
    for id in p["broken"].as_array().into_iter().flatten() {
        match id.as_str().filter(|id| player_id(id)) {
            Some(id) => {
                players.broken.insert(id.to_owned());
            }
            None => return Err(bad("players.broken")),
        }
    }
    if let Some(pin) = p.get("pin").filter(|p| !p.is_null()) {
        let id = pin["id"].as_str().filter(|id| player_id(id));
        let sts = pin["sts"].as_u64().and_then(valid_sts);
        match (id, sts) {
            (Some(id), Some(sts)) => {
                players.pin = Some(PinnedPlayer {
                    id: id.to_owned(),
                    sts,
                    force: pin["force"].as_bool().unwrap_or(false),
                })
            }
            _ => return Err(bad("players.pin")),
        }
    }
    Ok(PlayerFixes {
        serial,
        issued,
        clients,
        order,
        disabled,
        players,
    })
}

/// Printable ASCII without control characters (header-safe), at most `max` long.
fn plain(s: &str, max: usize) -> bool {
    s.len() <= max && s.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
}

fn player_id(id: &str) -> bool {
    (4..=32).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_sts(sts: u64) -> Option<u32> {
    (10_000..=99_999).contains(&sts).then_some(sts as u32)
}

/// Checks `signature` (a `.minisig` file) over `manifest` with `public_key`
/// and parses the manifest. Anything unsigned, signed by another key, or
/// altered is an error.
pub fn verify_and_parse(manifest: &[u8], signature: &str, public_key: &str) -> Result<PlayerFixes> {
    let key = minisign::PublicKey::decode(public_key)?;
    minisign::verify(&key, manifest, signature)?;
    parse(manifest)
}

/// The manifest on disk and where to fetch new ones.
pub struct FixesStore {
    dir: PathBuf,
    /// Manifest URLs, tried in order; each one's signature is at `<url>.minisig`.
    urls: Vec<String>,
    public_key: String,
}

impl FixesStore {
    pub fn new(dir: PathBuf, urls: Vec<String>) -> Self {
        Self {
            dir,
            urls,
            public_key: PUBLIC_KEY.to_owned(),
        }
    }

    #[cfg(test)]
    fn with_key(mut self, key: &str) -> Self {
        self.public_key = key.to_owned();
        self
    }

    /// The cached manifest, if it is there and its signature verifies.
    pub fn cached(&self) -> Option<PlayerFixes> {
        let manifest = std::fs::read(self.dir.join(FILE)).ok()?;
        let signature =
            std::fs::read_to_string(self.dir.join(format!("{FILE}{SIG_SUFFIX}"))).ok()?;
        match verify_and_parse(&manifest, &signature, &self.public_key) {
            Ok(fixes) => Some(fixes),
            Err(e) => {
                log::warn!("youtube: ignoring the cached player fixes: {e}");
                None
            }
        }
    }

    /// Whether the last check is older than [`CHECK_EVERY`].
    pub fn due(&self, now: SystemTime) -> bool {
        std::fs::metadata(self.dir.join(CHECKED))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_none_or(|age| age >= CHECK_EVERY)
    }

    /// Fetches the manifest from the first URL that serves a validly signed one.
    /// Returns it (and caches it) if its serial is above `current`; `Ok(None)`
    /// if it's not newer. Unsigned or bad content is ignored (an error if no URL
    /// served a valid one).
    pub fn fetch(&self, current: u64) -> Result<Option<PlayerFixes>> {
        self.fetch_with(current, fetch_small)
    }

    fn fetch_with(
        &self,
        current: u64,
        mut get: impl FnMut(&str) -> Result<Vec<u8>>,
    ) -> Result<Option<PlayerFixes>> {
        let mut last = None;
        for url in &self.urls {
            let fetched = get(url).and_then(|manifest| {
                let signature = get(&format!("{url}{SIG_SUFFIX}"))?;
                let signature = String::from_utf8(signature)
                    .map_err(|_| Error::Parse("player fixes: signature is not text".into()))?;
                let fixes = verify_and_parse(&manifest, &signature, &self.public_key)?;
                Ok((manifest, signature, fixes))
            });
            match fetched {
                Ok((manifest, signature, fixes)) => {
                    self.mark_checked();
                    if fixes.serial <= current {
                        log::debug!("youtube: player fixes {} are current", fixes.serial);
                        return Ok(None);
                    }
                    self.store(&manifest, &signature);
                    log::info!(
                        "youtube: player fixes {} ({}) from {url}",
                        fixes.serial,
                        fixes.issued
                    );
                    return Ok(Some(fixes));
                }
                Err(e) => {
                    log::warn!("youtube: player fixes from {url}: {e}");
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| Error::Parse("player fixes: no source".into())))
    }

    fn store(&self, manifest: &[u8], signature: &str) {
        let _ = std::fs::create_dir_all(&self.dir);
        // Signature last: a half-written pair fails verification and is ignored.
        let _ = std::fs::remove_file(self.dir.join(format!("{FILE}{SIG_SUFFIX}")));
        let _ = write_atomic(&self.dir.join(FILE), manifest);
        let _ = write_atomic(
            &self.dir.join(format!("{FILE}{SIG_SUFFIX}")),
            signature.as_bytes(),
        );
    }

    fn mark_checked(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join(CHECKED), b"");
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

fn fetch_small(url: &str) -> Result<Vec<u8>> {
    let mut resp = agent()
        .get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .call()?;
    let body = read_body(&mut resp)?;
    if body.len() > MAX_BYTES {
        return Err(Error::Parse(format!("player fixes: {url} is too large")));
    }
    Ok(body)
}

/// [minisign](https://jedisct1.github.io/minisign/)-compatible Ed25519 keys and
/// signatures (prehashed `ED` signatures; secret keys without a password).
pub mod minisign {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use blake2::digest::{Update, VariableOutput};
    use blake2::{Blake2b512, Blake2bVar, Digest as _};
    use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};

    use crate::{Error, Result};

    fn bad(what: &str) -> Error {
        Error::Parse(format!("minisign: {what}"))
    }

    /// A public key (`RW…`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PublicKey {
        pub key_id: [u8; 8],
        pub key: [u8; 32],
    }

    impl PublicKey {
        /// From the base64 line (`RW…`) or a whole `.pub` file.
        pub fn decode(text: &str) -> Result<Self> {
            let line = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
                .ok_or_else(|| bad("empty public key"))?;
            let raw = STANDARD
                .decode(line)
                .map_err(|_| bad("public key base64"))?;
            if raw.len() != 42 || &raw[..2] != b"Ed" {
                return Err(bad("not an Ed25519 public key"));
            }
            Ok(Self {
                key_id: raw[2..10].try_into().expect("8 bytes"),
                key: raw[10..42].try_into().expect("32 bytes"),
            })
        }

        /// The base64 line (`RW…`).
        pub fn encode(&self) -> String {
            let mut raw = b"Ed".to_vec();
            raw.extend_from_slice(&self.key_id);
            raw.extend_from_slice(&self.key);
            STANDARD.encode(raw)
        }

        /// minisign's key id display (`%016X` of the little-endian number).
        pub fn id_hex(&self) -> String {
            format!("{:016X}", u64::from_le_bytes(self.key_id))
        }

        /// The `.pub` file minisign writes.
        pub fn to_file(&self) -> String {
            format!(
                "untrusted comment: minisign public key {}\n{}\n",
                self.id_hex(),
                self.encode()
            )
        }
    }

    /// A secret key (seed + public key, libsodium layout) with its id.
    pub struct SecretKey {
        pub key_id: [u8; 8],
        /// seed (32) || public key (32).
        sk: [u8; 64],
    }

    impl SecretKey {
        /// A new random key pair.
        pub fn generate() -> Result<Self> {
            let mut seed = [0u8; 32];
            let mut key_id = [0u8; 8];
            getrandom::fill(&mut seed).map_err(|e| bad(&format!("random: {e}")))?;
            getrandom::fill(&mut key_id).map_err(|e| bad(&format!("random: {e}")))?;
            Self::from_seed(seed, key_id)
        }

        pub fn from_seed(seed: [u8; 32], key_id: [u8; 8]) -> Result<Self> {
            let pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| bad("bad seed"))?;
            let mut sk = [0u8; 64];
            sk[..32].copy_from_slice(&seed);
            sk[32..].copy_from_slice(pair.public_key().as_ref());
            Ok(Self { key_id, sk })
        }

        pub fn public(&self) -> PublicKey {
            PublicKey {
                key_id: self.key_id,
                key: self.sk[32..].try_into().expect("32 bytes"),
            }
        }

        fn pair(&self) -> Result<Ed25519KeyPair> {
            Ed25519KeyPair::from_seed_and_public_key(&self.sk[..32], &self.sk[32..])
                .map_err(|_| bad("inconsistent secret key"))
        }

        /// minisign's unencrypted secret key file (`minisign -G -W` layout):
        /// `Ed` `\0\0` `B2` salt(32) opslimit(8) memlimit(8) keynum(8) sk(64) checksum(32).
        pub fn to_file(&self) -> String {
            let mut raw = b"Ed\0\0B2".to_vec();
            raw.extend_from_slice(&[0u8; 48]);
            raw.extend_from_slice(&self.key_id);
            raw.extend_from_slice(&self.sk);
            raw.extend_from_slice(&self.checksum());
            format!(
                "untrusted comment: minisign secret key (unencrypted; keep it out of the repository)\n{}\n",
                STANDARD.encode(raw)
            )
        }

        pub fn from_file(text: &str) -> Result<Self> {
            let line = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
                .ok_or_else(|| bad("empty secret key"))?;
            let raw = STANDARD
                .decode(line)
                .map_err(|_| bad("secret key base64"))?;
            if raw.len() != 158 || &raw[..2] != b"Ed" || &raw[4..6] != b"B2" {
                return Err(bad("not an Ed25519 secret key"));
            }
            if raw[2..4] != [0, 0] {
                return Err(bad(
                    "password-protected keys aren't supported here (use `minisign -S`)",
                ));
            }
            let key = Self {
                key_id: raw[54..62].try_into().expect("8 bytes"),
                sk: raw[62..126].try_into().expect("64 bytes"),
            };
            if key.checksum()[..] != raw[126..158] {
                return Err(bad("secret key checksum mismatch"));
            }
            key.pair()?;
            Ok(key)
        }

        /// BLAKE2b-256 of `Ed` || key id || sk, as minisign checks it.
        fn checksum(&self) -> [u8; 32] {
            let mut h = Blake2bVar::new(32).expect("valid length");
            h.update(b"Ed");
            h.update(&self.key_id);
            h.update(&self.sk);
            let mut out = [0u8; 32];
            h.finalize_variable(&mut out).expect("32 bytes");
            out
        }

        /// A `.minisig` file for `message` (prehashed), with `trusted_comment`.
        pub fn sign(&self, message: &[u8], trusted_comment: &str) -> Result<String> {
            if trusted_comment.contains(['\n', '\r']) {
                return Err(bad("the trusted comment must be one line"));
            }
            let pair = self.pair()?;
            let sig = pair.sign(&Blake2b512::digest(message));
            let mut global = sig.as_ref().to_vec();
            global.extend_from_slice(trusted_comment.as_bytes());
            let global = pair.sign(&global);
            let mut line = b"ED".to_vec();
            line.extend_from_slice(&self.key_id);
            line.extend_from_slice(sig.as_ref());
            Ok(format!(
                "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: {trusted_comment}\n{}\n",
                STANDARD.encode(line),
                STANDARD.encode(global.as_ref())
            ))
        }
    }

    /// Verifies a `.minisig` over `message`; returns its trusted comment.
    pub fn verify(key: &PublicKey, message: &[u8], signature: &str) -> Result<String> {
        let mut lines = signature.lines().map(|l| l.trim_end_matches('\r'));
        let mut next = || lines.next().ok_or_else(|| bad("truncated signature"));
        if !next()?.starts_with("untrusted comment:") {
            return Err(bad("no untrusted comment"));
        }
        let raw = STANDARD
            .decode(next()?.trim())
            .map_err(|_| bad("signature base64"))?;
        let trusted = next()?
            .strip_prefix("trusted comment: ")
            .ok_or_else(|| bad("no trusted comment"))?
            .to_owned();
        let global = STANDARD
            .decode(next()?.trim())
            .map_err(|_| bad("global signature base64"))?;
        if raw.len() != 74 || global.len() != 64 {
            return Err(bad("malformed signature"));
        }
        if &raw[..2] != b"ED" {
            return Err(bad("only prehashed (ED) signatures are accepted"));
        }
        if raw[2..10] != key.key_id {
            return Err(bad("signed with another key"));
        }
        let public = UnparsedPublicKey::new(&ED25519, key.key);
        let sig = &raw[10..74];
        public
            .verify(&Blake2b512::digest(message), sig)
            .map_err(|_| bad("signature verification failed"))?;
        let mut signed = sig.to_vec();
        signed.extend_from_slice(trusted.as_bytes());
        public
            .verify(&signed, &global)
            .map_err(|_| bad("trusted comment verification failed"))?;
        Ok(trusted)
    }
}

#[cfg(test)]
mod tests {
    use super::minisign::{PublicKey, SecretKey};
    use super::*;

    fn key() -> SecretKey {
        SecretKey::from_seed([7; 32], *b"testkey!").unwrap()
    }

    const MANIFEST: &str = r#"{
        "schema": 1, "serial": 5, "issued": "2026-10-31",
        "clients": {
            "VISIONOS": { "clientVersion": "1.03", "userAgent": "UA/1", "evil": "x" },
            "NOT_A_CLIENT": { "clientVersion": "9" }
        },
        "order": ["TVHTML5", "VISIONOS", "BOGUS"],
        "disabled": ["MWEB"],
        "players": {
            "sts": { "abcd1234": 20500 },
            "broken": ["deadbeef"],
            "pin": { "id": "abcd1234", "sts": 20500 }
        },
        "future": { "anything": true }
    }"#;

    #[test]
    fn signature_verifies_and_rejects() {
        let sk = key();
        let pk = sk.public();
        let sig = sk
            .sign(MANIFEST.as_bytes(), "timestamp:1\tfile:player-fixes.json")
            .unwrap();
        let fixes = verify_and_parse(MANIFEST.as_bytes(), &sig, &pk.encode()).unwrap();
        assert_eq!(fixes.serial, 5);
        assert_eq!(
            minisign::verify(&pk, MANIFEST.as_bytes(), &sig).unwrap(),
            "timestamp:1\tfile:player-fixes.json"
        );
        // The .pub file form decodes too.
        assert_eq!(PublicKey::decode(&pk.to_file()).unwrap(), pk);

        // Altered content.
        let altered = MANIFEST.replace("20500", "20501");
        assert!(verify_and_parse(altered.as_bytes(), &sig, &pk.encode()).is_err());
        // Altered trusted comment.
        let forged = sig.replace("timestamp:1", "timestamp:2");
        assert!(verify_and_parse(MANIFEST.as_bytes(), &forged, &pk.encode()).is_err());
        // Another key (same id or not).
        let other = SecretKey::from_seed([8; 32], *b"testkey!").unwrap();
        let other_sig = other.sign(MANIFEST.as_bytes(), "x").unwrap();
        assert!(verify_and_parse(MANIFEST.as_bytes(), &other_sig, &pk.encode()).is_err());
        let other = SecretKey::from_seed([8; 32], *b"otherkey").unwrap();
        let other_sig = other.sign(MANIFEST.as_bytes(), "x").unwrap();
        assert!(verify_and_parse(MANIFEST.as_bytes(), &other_sig, &pk.encode()).is_err());
        // Unsigned / garbage.
        assert!(verify_and_parse(MANIFEST.as_bytes(), "", &pk.encode()).is_err());
        assert!(
            verify_and_parse(
                MANIFEST.as_bytes(),
                "untrusted comment: x\nAAAA\n",
                &pk.encode()
            )
            .is_err()
        );
    }

    #[test]
    fn secret_key_file_round_trip() {
        let sk = key();
        let file = sk.to_file();
        let back = SecretKey::from_file(&file).unwrap();
        assert_eq!(back.public(), sk.public());
        // A damaged key is refused.
        let mut lines: Vec<String> = file.lines().map(str::to_owned).collect();
        let mut raw =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &lines[1]).unwrap();
        raw[70] ^= 1;
        lines[1] = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
        assert!(SecretKey::from_file(&lines.join("\n")).is_err());
    }

    /// The built-in manifest parses, and the compiled-in key is well-formed.
    #[test]
    fn bundled_manifest_and_key() {
        let bundled = parse(BUNDLED.as_bytes()).unwrap();
        assert!(bundled.serial >= 1);
        assert_eq!(PlayerFixes::bundled(), bundled);
        PublicKey::decode(PUBLIC_KEY).unwrap();
    }

    #[test]
    fn manifest_fields_are_validated() {
        let f = parse(MANIFEST.as_bytes()).unwrap();
        let vision = &f.clients.0[&WebClient::VisionOs];
        assert_eq!(vision["clientVersion"], "1.03");
        assert!(!vision.contains_key("evil"), "only known fields");
        assert_eq!(f.clients.0.len(), 1);
        assert_eq!(f.order, [WebClient::Tv, WebClient::VisionOs]);
        assert!(f.disabled.contains(&WebClient::MobileWeb));
        assert_eq!(f.players.sts["abcd1234"], 20500);

        // Order and disabled clients.
        let enabled = WebClient::ALL;
        assert_eq!(
            f.arrange(&enabled),
            [WebClient::Tv, WebClient::VisionOs, WebClient::TvSimply]
        );
        assert_eq!(
            f.arrange(&[WebClient::MobileWeb]),
            [WebClient::MobileWeb],
            "never none"
        );

        // Players: a broken one is swapped for the pin; others keep theirs.
        assert_eq!(
            f.players.choose("deadbeef"),
            ("abcd1234".into(), Some(20500))
        );
        assert_eq!(f.players.choose("12345678"), ("12345678".into(), None));
        assert_eq!(
            f.players.choose("abcd1234"),
            ("abcd1234".into(), Some(20500))
        );

        for bad in [
            r#"{"schema": 2, "serial": 1}"#,
            r#"{"schema": 1}"#,
            r#"{"schema": 1, "serial": 1, "clients": {"TVHTML5": {"userAgent": "a\r\nX-Evil: 1"}}}"#,
            r#"{"schema": 1, "serial": 1, "players": {"sts": {"../../x": 20000}}}"#,
            r#"{"schema": 1, "serial": 1, "players": {"sts": {"abcd1234": 7}}}"#,
            r#"{"schema": 1, "serial": 1, "players": {"pin": {"id": "abcd1234"}}}"#,
            "not json",
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn store_caches_only_verified_newer_manifests() {
        let dir = std::env::temp_dir().join(format!("music-core-fixes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sk = key();
        let pk = sk.public().encode();
        let store = FixesStore::new(
            dir.clone(),
            vec![
                "https://bad/player-fixes.json".into(),
                "https://good/player-fixes.json".into(),
            ],
        )
        .with_key(&pk);
        assert!(store.cached().is_none());
        assert!(store.due(SystemTime::now()));

        let sig = sk.sign(MANIFEST.as_bytes(), "t").unwrap();
        let unsigned_mirror = |url: &str| -> Result<Vec<u8>> {
            if url.starts_with("https://bad/") {
                // A mirror serving altered content with the real signature.
                if url.ends_with(".minisig") {
                    Ok(sig.clone().into_bytes())
                } else {
                    Ok(MANIFEST.replace("1.03", "6.66").into_bytes())
                }
            } else if url.ends_with(".minisig") {
                Ok(sig.clone().into_bytes())
            } else {
                Ok(MANIFEST.as_bytes().to_vec())
            }
        };
        // The bad mirror is skipped; the good one is used and cached.
        let got = store.fetch_with(1, unsigned_mirror).unwrap().unwrap();
        assert_eq!(got.serial, 5);
        assert_eq!(got.clients.0[&WebClient::VisionOs]["clientVersion"], "1.03");
        assert_eq!(store.cached().unwrap(), got);
        assert!(!store.due(SystemTime::now()));
        assert!(store.due(SystemTime::now() + CHECK_EVERY));

        // Not newer: not replaced.
        assert!(store.fetch_with(5, unsigned_mirror).unwrap().is_none());
        // Every source bad: an error, the cache stays.
        let all_bad = |_: &str| -> Result<Vec<u8>> { Ok(b"{}".to_vec()) };
        assert!(store.fetch_with(1, all_bad).is_err());
        assert_eq!(store.cached().unwrap().serial, 5);

        // A tampered cache is ignored.
        std::fs::write(dir.join(FILE), MANIFEST.replace("1.03", "6.66")).unwrap();
        assert!(store.cached().is_none());
        // So is one checked against another key.
        let other = FixesStore::new(dir.clone(), vec![]).with_key(
            &SecretKey::from_seed([9; 32], [1; 8])
                .unwrap()
                .public()
                .encode(),
        );
        std::fs::write(dir.join(FILE), MANIFEST).unwrap();
        assert!(other.cached().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
