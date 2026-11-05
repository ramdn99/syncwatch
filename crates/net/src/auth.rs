//! Peer authentication (spec section 9 "Handshake and auth", ADR 0006). Pure: no I/O.
//!
//! * Every install has an ed25519 identity ([`IdentityKey`]); its public key is the member's
//!   [`Identity`] (bans and seats are keyed by it).
//! * The transcript hash binds the room code, BOTH DTLS fingerprints (member = offerer, host =
//!   answerer), both identities, the member's name, both nonces and the SPAKE2 messages. A
//!   man in the middle terminates DTLS separately on each side, so the two sides would hash
//!   different fingerprints.
//! * Password rooms: SPAKE2 (Ed25519 group) with the room password. The member confirms first,
//!   so a wrong password is always seen by the host and throttled ([`AuthThrottle`]); the host
//!   never sends a confirmation an attacker could use for an offline guess.
//! * Rooms without a password: both sides show four verification words derived from the
//!   transcript; if they match, nobody is in the middle.
//! * Both sides sign the transcript with their identity key, binding the identity to this
//!   DTLS session.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};
use std::collections::{HashMap, VecDeque};
use syncwatch_protocol::Identity;
use syncwatch_protocol::auth::*;

// ------------------------------------------------------------------------------ identity ----

/// The per-install ed25519 key (stored beside the connection credentials by the app).
#[derive(Clone)]
pub struct IdentityKey(SigningKey);

impl std::fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdentityKey({})", short_id(&self.public()))
    }
}

impl IdentityKey {
    /// A new random key (OS randomness).
    pub fn generate() -> Option<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).ok()?;
        Some(Self(SigningKey::from_bytes(&secret)))
    }

    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&secret))
    }

    /// The secret bytes, for storage in the credential store only.
    pub fn secret(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public(&self) -> Identity {
        Identity(self.0.verifying_key().to_bytes())
    }

    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.0.sign(msg).to_bytes().to_vec()
    }
}

fn verify(id: &Identity, msg: &[u8], sig: &[u8]) -> bool {
    let Ok(sig) = <[u8; 64]>::try_from(sig) else {
        return false;
    };
    VerifyingKey::from_bytes(&id.0).is_ok_and(|k| {
        k.verify_strict(msg, &ed25519_dalek::Signature::from_bytes(&sig))
            .is_ok()
    })
}

/// Short printable form of an identity for logs (first 4 bytes, not secret).
pub fn short_id(id: &Identity) -> String {
    id.0[..4].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn random_nonce() -> [u8; NONCE_LEN] {
    let mut n = [0u8; NONCE_LEN];
    // A failure here only weakens freshness, not secrecy; fall back to a time-derived value.
    if getrandom::fill(&mut n).is_err() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        n = *blake3::hash(&t.to_le_bytes()).as_bytes();
    }
    n
}

// ------------------------------------------------------------------------------ words -------

/// 256 short, distinct, easy-to-say words (4 words = 32 bits).
pub const WORDS: [&str; 256] = [
    "acorn", "anchor", "apple", "arrow", "badge", "bamboo", "banjo", "barrel", "basket", "beacon",
    "bell", "bench", "berry", "bison", "blade", "bloom", "boat", "bonnet", "bottle", "branch",
    "bread", "brick", "bridge", "brush", "bucket", "button", "cabin", "cactus", "camel", "candle",
    "canoe", "canyon", "carrot", "castle", "cedar", "chair", "cherry", "chess", "cider", "circle",
    "clock", "cloud", "clover", "coast", "cobra", "comet", "copper", "coral", "corn", "cotton",
    "crane", "crayon", "cricket", "crown", "crystal", "cup", "daisy", "delta", "desert", "diamond",
    "dolphin", "donkey", "dragon", "drum", "eagle", "earth", "easel", "echo", "elbow", "ember",
    "engine", "falcon", "feather", "fern", "ferry", "fiddle", "field", "finch", "flame", "flute",
    "forest", "fossil", "fountain", "fox", "frost", "galaxy", "garden", "garlic", "gecko",
    "geyser", "ginger", "glacier", "globe", "goat", "grape", "gravel", "guitar", "hammer",
    "harbor", "harp", "hazel", "helmet", "heron", "hill", "honey", "horizon", "horse", "island",
    "ivory", "jacket", "jaguar", "jasmine", "jelly", "jungle", "kayak", "kettle", "kite", "koala",
    "ladder", "lagoon", "lake", "lantern", "lemon", "lily", "lion", "lizard", "llama", "lobster",
    "locket", "lotus", "magnet", "mango", "maple", "marble", "meadow", "melon", "mirror", "mitten",
    "moon", "moose", "mosaic", "mountain", "mule", "napkin", "needle", "nest", "nickel", "noodle",
    "oak", "oasis", "ocean", "olive", "onion", "orange", "orbit", "orchid", "otter", "owl",
    "oyster", "paddle", "palace", "panda", "paper", "parrot", "peach", "pearl", "pebble",
    "pelican", "pencil", "pepper", "piano", "pigeon", "pillow", "pine", "planet", "plum", "pocket",
    "pony", "poppy", "prism", "pumpkin", "puzzle", "quail", "quartz", "quill", "rabbit", "radar",
    "rainbow", "raven", "reef", "ribbon", "river", "robin", "rocket", "rose", "ruby", "saddle",
    "salmon", "sand", "satchel", "scarf", "seal", "shell", "ship", "silver", "sketch", "sled",
    "snail", "snow", "sparrow", "spider", "spoon", "spruce", "squid", "star", "stone", "storm",
    "summit", "sun", "swan", "table", "tablet", "teapot", "temple", "thistle", "thunder", "tiger",
    "timber", "toast", "tomato", "topaz", "torch", "tower", "trail", "train", "tulip", "tunnel",
    "turtle", "umbrella", "valley", "velvet", "violet", "violin", "volcano", "wagon", "walnut",
    "walrus", "wave", "whale", "wheat", "willow", "window", "wolf", "wren", "yacht", "zebra",
];

pub type VerificationWords = [&'static str; 4];

fn words_for(transcript: &[u8; 32]) -> VerificationWords {
    let h = blake3::derive_key("syncwatch auth v1 verification words", transcript);
    [
        WORDS[h[0] as usize],
        WORDS[h[1] as usize],
        WORDS[h[2] as usize],
        WORDS[h[3] as usize],
    ]
}

// ------------------------------------------------------------------------------ transcript --

/// DTLS fingerprints of one connection, in canonical roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprints {
    /// The member's (offerer's) certificate fingerprint.
    pub member: String,
    /// The host's (answerer's) certificate fingerprint.
    pub host: String,
}

struct TranscriptInput<'a> {
    room: &'a str,
    fps: &'a Fingerprints,
    host_identity: &'a Identity,
    host_nonce: &'a [u8; NONCE_LEN],
    password_required: bool,
    member_identity: &'a Identity,
    member_name: &'a str,
    member_nonce: &'a [u8; NONCE_LEN],
    spake_a: &'a [u8],
    spake_b: &'a [u8],
}

fn transcript(t: &TranscriptInput<'_>) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("syncwatch auth v1 transcript");
    let mut field = |b: &[u8]| {
        h.update(&(b.len() as u64).to_le_bytes());
        h.update(b);
    };
    field(t.room.as_bytes());
    field(t.fps.member.as_bytes());
    field(t.fps.host.as_bytes());
    field(&t.host_identity.0);
    field(t.host_nonce);
    field(&[t.password_required as u8]);
    field(&t.member_identity.0);
    field(t.member_name.as_bytes());
    field(t.member_nonce);
    field(t.spake_a);
    field(t.spake_b);
    *h.finalize().as_bytes()
}

fn mac(key: &[u8], side: &str, t: &[u8; 32]) -> [u8; 32] {
    let mut material = Vec::with_capacity(key.len() + 32);
    material.extend_from_slice(key);
    material.extend_from_slice(t);
    let k = blake3::derive_key(&format!("syncwatch auth v1 {side} mac"), &material);
    *blake3::keyed_hash(&k, t).as_bytes()
}

fn sig_msg(side: &str, t: &[u8; 32]) -> Vec<u8> {
    let mut m = format!("syncwatch-auth-v1 {side} ").into_bytes();
    m.extend_from_slice(t);
    m
}

/// Constant-time comparison of two MACs.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn spake_ids(member: &Identity, host: &Identity) -> (SpakeId, SpakeId) {
    (SpakeId::new(&member.0), SpakeId::new(&host.0))
}

// ------------------------------------------------------------------------------ throttle ----

/// Wrong-password throttling on the host (spec section 9): slow down after each failure,
/// temporarily ban an identity after 6 failures, and pause all password attempts in the room
/// after 20 failures within a minute (identities are free to create, so the global limit is
/// what bounds an online guessing attack).
#[derive(Debug, Default)]
pub struct AuthThrottle {
    per_identity: HashMap<Identity, IdentityFailures>,
    recent: VecDeque<i64>,
    paused_until: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct IdentityFailures {
    count: u32,
    next_allowed: i64,
    banned_until: Option<i64>,
}

pub const BAN_AFTER_FAILURES: u32 = 6;
pub const BAN_US: i64 = 10 * 60 * 1_000_000;
pub const GLOBAL_FAILURES: usize = 20;
pub const GLOBAL_WINDOW_US: i64 = 60 * 1_000_000;
pub const GLOBAL_PAUSE_US: i64 = 60 * 1_000_000;
pub const MAX_BACKOFF_US: i64 = 8 * 1_000_000;

impl AuthThrottle {
    /// May this identity try a password now? `Err((reason, retry_after_ms))` otherwise.
    pub fn check(&mut self, id: &Identity, now: i64) -> Result<(), (AuthReject, u32)> {
        if let Some(until) = self.paused_until {
            if now < until {
                return Err((AuthReject::TooManyAttempts, ms(until - now)));
            }
            self.paused_until = None;
        }
        if let Some(f) = self.per_identity.get(id) {
            if let Some(until) = f.banned_until
                && now < until
            {
                return Err((AuthReject::TooManyAttempts, 0));
            }
            if now < f.next_allowed {
                return Err((AuthReject::TooManyAttempts, ms(f.next_allowed - now)));
            }
        }
        Ok(())
    }

    /// Records a wrong password. Returns the reject to send: BadPassword with the delay before
    /// the next attempt, or TooManyAttempts (0 = the connection is closed).
    pub fn record_failure(&mut self, id: &Identity, now: i64) -> (AuthReject, u32) {
        self.recent.push_back(now);
        while self
            .recent
            .front()
            .is_some_and(|t| now - t > GLOBAL_WINDOW_US)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= GLOBAL_FAILURES {
            self.paused_until = Some(now + GLOBAL_PAUSE_US);
            tracing::warn!(
                failures = self.recent.len(),
                "too many wrong passwords in a minute; pausing password attempts"
            );
        }
        let f = self.per_identity.entry(*id).or_default();
        f.count += 1;
        if f.count >= BAN_AFTER_FAILURES {
            f.banned_until = Some(now + BAN_US);
            return (AuthReject::TooManyAttempts, 0);
        }
        let backoff = (1_000_000i64 << (f.count - 1).min(4)).min(MAX_BACKOFF_US);
        f.next_allowed = now + backoff;
        if let Some(until) = self.paused_until {
            return (AuthReject::TooManyAttempts, ms(until - now));
        }
        (AuthReject::BadPassword, ms(backoff))
    }

    pub fn record_success(&mut self, id: &Identity) {
        self.per_identity.remove(id);
    }
}

fn ms(us: i64) -> u32 {
    (us.max(0) / 1000).min(u32::MAX as i64) as u32
}

// ------------------------------------------------------------------------------ host side ---

/// What the host side knows about its room.
#[derive(Clone)]
pub struct HostAuthConfig {
    pub room: String,
    pub key: IdentityKey,
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStep {
    /// Nothing to do (yet).
    Wait,
    Send(Vec<AuthMsg>),
    /// The member is authenticated; hand it to the room (which may still reject it as
    /// banned, locked or full).
    Authenticated {
        identity: Identity,
        display_name: String,
        words: VerificationWords,
        send: Vec<AuthMsg>,
    },
    /// Send these, then close the connection if `close`.
    Reject {
        send: Vec<AuthMsg>,
        close: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostState {
    AwaitHello,
    AwaitConfirm,
    Done,
    Failed,
}

struct MemberClaim {
    identity: Identity,
    name: String,
    nonce: [u8; NONCE_LEN],
    spake_a: Vec<u8>,
    spake_b: Vec<u8>,
    key: Vec<u8>,
}

/// One connection's handshake, host side.
pub struct HostAuth {
    state: HostState,
    fps: Fingerprints,
    nonce: [u8; NONCE_LEN],
    claim: Option<MemberClaim>,
}

impl HostAuth {
    pub fn start(
        cfg: &HostAuthConfig,
        fps: Fingerprints,
        nonce: [u8; NONCE_LEN],
    ) -> (Self, AuthMsg) {
        let hello = AuthMsg::HostHello {
            version: AUTH_VERSION,
            host_identity: cfg.key.public(),
            password_required: cfg.password.is_some(),
            nonce,
        };
        (
            Self {
                state: HostState::AwaitHello,
                fps,
                nonce,
                claim: None,
            },
            hello,
        )
    }

    pub fn is_done(&self) -> bool {
        self.state == HostState::Done
    }

    fn fail(&mut self, reason: AuthReject) -> HostStep {
        self.state = HostState::Failed;
        HostStep::Reject {
            send: vec![AuthMsg::Rejected {
                reason,
                retry_after_ms: 0,
            }],
            close: true,
        }
    }

    pub fn on_msg(
        &mut self,
        cfg: &HostAuthConfig,
        throttle: &mut AuthThrottle,
        now: i64,
        msg: AuthMsg,
    ) -> HostStep {
        match (self.state, msg) {
            (
                HostState::AwaitHello,
                AuthMsg::MemberHello {
                    version,
                    identity,
                    display_name,
                    nonce,
                    spake,
                },
            ) => {
                if version != AUTH_VERSION {
                    return self.fail(AuthReject::Version);
                }
                let host_id = cfg.key.public();
                let (spake_a, spake_b, key) = match &cfg.password {
                    None => (Vec::new(), Vec::new(), Vec::new()),
                    Some(pw) => {
                        if let Err((reason, retry)) = throttle.check(&identity, now) {
                            return HostStep::Reject {
                                send: vec![AuthMsg::Rejected {
                                    reason,
                                    retry_after_ms: retry,
                                }],
                                close: retry == 0,
                            };
                        }
                        let Some(a) = spake else {
                            let (reason, retry) = throttle.record_failure(&identity, now);
                            return HostStep::Reject {
                                send: vec![AuthMsg::Rejected {
                                    reason,
                                    retry_after_ms: retry,
                                }],
                                close: retry == 0,
                            };
                        };
                        let (ida, idb) = spake_ids(&identity, &host_id);
                        let (s, b) = Spake2::<Ed25519Group>::start_b(
                            &Password::new(pw.as_bytes()),
                            &ida,
                            &idb,
                        );
                        let Ok(key) = s.finish(&a) else {
                            return self.fail(AuthReject::Failed);
                        };
                        (a, b, key)
                    }
                };
                let send = if spake_b.is_empty() {
                    Vec::new()
                } else {
                    vec![AuthMsg::HostSpake {
                        spake: spake_b.clone(),
                    }]
                };
                self.claim = Some(MemberClaim {
                    identity,
                    name: display_name,
                    nonce,
                    spake_a,
                    spake_b,
                    key,
                });
                self.state = HostState::AwaitConfirm;
                if send.is_empty() {
                    HostStep::Wait
                } else {
                    HostStep::Send(send)
                }
            }
            (HostState::AwaitConfirm, AuthMsg::MemberConfirm { mac: their, sig }) => {
                let Some(c) = self.claim.take() else {
                    return self.fail(AuthReject::Failed);
                };
                let host_id = cfg.key.public();
                let t = transcript(&TranscriptInput {
                    room: &cfg.room,
                    fps: &self.fps,
                    host_identity: &host_id,
                    host_nonce: &self.nonce,
                    password_required: cfg.password.is_some(),
                    member_identity: &c.identity,
                    member_name: &c.name,
                    member_nonce: &c.nonce,
                    spake_a: &c.spake_a,
                    spake_b: &c.spake_b,
                });
                let key = session_key(&c.key, &t);
                let mac_ok = ct_eq(&mac(&key, "member", &t), &their);
                if !mac_ok {
                    if cfg.password.is_some() {
                        // Wrong password: the member may try again on this connection later.
                        let (reason, retry) = throttle.record_failure(&c.identity, now);
                        tracing::info!(
                            member = %short_id(&c.identity),
                            retry_ms = retry,
                            "wrong room password"
                        );
                        self.state = if retry == 0 {
                            HostState::Failed
                        } else {
                            HostState::AwaitHello
                        };
                        return HostStep::Reject {
                            send: vec![AuthMsg::Rejected {
                                reason,
                                retry_after_ms: retry,
                            }],
                            close: retry == 0,
                        };
                    }
                    return self.fail(AuthReject::Failed);
                }
                if !verify(&c.identity, &sig_msg("member", &t), &sig) {
                    return self.fail(AuthReject::Failed);
                }
                throttle.record_success(&c.identity);
                self.state = HostState::Done;
                HostStep::Authenticated {
                    identity: c.identity,
                    display_name: c.name,
                    words: words_for(&t),
                    send: vec![AuthMsg::HostConfirm {
                        mac: mac(&key, "host", &t),
                        sig: cfg.key.sign(&sig_msg("host", &t)),
                    }],
                }
            }
            (HostState::Done, _) | (HostState::Failed, _) => HostStep::Wait,
            _ => self.fail(AuthReject::Failed),
        }
    }
}

/// Without a password the key is public (derived from the transcript); the MACs then only
/// check transcript agreement, and the words plus the signatures carry the authentication.
fn session_key(spake_key: &[u8], t: &[u8; 32]) -> Vec<u8> {
    if spake_key.is_empty() {
        blake3::derive_key("syncwatch auth v1 no password", t).to_vec()
    } else {
        spake_key.to_vec()
    }
}

// ------------------------------------------------------------------------------ member side -

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberStep {
    Wait,
    Send(Vec<AuthMsg>),
    /// The room has a password and none was given: ask the user, then call
    /// [`MemberAuth::submit_password`].
    NeedPassword,
    Authenticated {
        host: Identity,
        words: VerificationWords,
        send: Vec<AuthMsg>,
    },
    /// The host refused. With `retry_after_ms > 0` another password may be tried on this
    /// connection after that delay; otherwise the connection is over.
    Rejected {
        reason: AuthReject,
        retry_after_ms: u32,
    },
    /// The host failed to prove itself (or misbehaved): close the connection.
    Failed(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberState {
    AwaitHostHello,
    NeedPassword,
    AwaitHostSpake,
    AwaitHostConfirm,
    Done,
    Failed,
}

pub struct MemberAuth {
    key: IdentityKey,
    name: String,
    room: String,
    password: Option<String>,
    fps: Fingerprints,
    /// After a host transfer: the identity the new host must have.
    expected_host: Option<Identity>,
    state: MemberState,
    host: Option<(Identity, [u8; NONCE_LEN], bool)>,
    nonce: [u8; NONCE_LEN],
    spake: Option<Spake2<Ed25519Group>>,
    spake_a: Vec<u8>,
    spake_b: Vec<u8>,
    transcript: Option<[u8; 32]>,
    session_key: Vec<u8>,
}

impl MemberAuth {
    pub fn new(
        key: IdentityKey,
        name: &str,
        room: &str,
        password: Option<String>,
        fps: Fingerprints,
        expected_host: Option<Identity>,
    ) -> Self {
        Self {
            key,
            name: name.to_owned(),
            room: room.to_owned(),
            password: password.filter(|p| !p.is_empty()),
            fps,
            expected_host,
            state: MemberState::AwaitHostHello,
            host: None,
            nonce: random_nonce(),
            spake: None,
            spake_a: Vec::new(),
            spake_b: Vec::new(),
            transcript: None,
            session_key: Vec::new(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.state == MemberState::Done
    }

    fn fail(&mut self, why: &'static str) -> MemberStep {
        self.state = MemberState::Failed;
        MemberStep::Failed(why)
    }

    fn hello(&mut self) -> MemberStep {
        let Some((host_id, _, password_required)) = self.host else {
            return self.fail("no host hello");
        };
        self.nonce = random_nonce();
        self.spake_b.clear();
        let spake = if password_required {
            let Some(pw) = &self.password else {
                self.state = MemberState::NeedPassword;
                return MemberStep::NeedPassword;
            };
            let (ida, idb) = spake_ids(&self.key.public(), &host_id);
            let (s, a) = Spake2::<Ed25519Group>::start_a(&Password::new(pw.as_bytes()), &ida, &idb);
            self.spake = Some(s);
            self.spake_a = a.clone();
            self.state = MemberState::AwaitHostSpake;
            Some(a)
        } else {
            self.spake_a.clear();
            self.state = MemberState::AwaitHostConfirm;
            None
        };
        let hello = AuthMsg::MemberHello {
            version: AUTH_VERSION,
            identity: self.key.public(),
            display_name: self.name.clone(),
            nonce: self.nonce,
            spake,
        };
        if password_required {
            MemberStep::Send(vec![hello])
        } else {
            // No SPAKE round: confirm right away.
            let confirm = self.confirm(Vec::new());
            MemberStep::Send(vec![hello, confirm])
        }
    }

    fn confirm(&mut self, spake_key: Vec<u8>) -> AuthMsg {
        let (host_id, host_nonce, password_required) =
            self.host
                .unwrap_or((Identity([0; 32]), [0; NONCE_LEN], false));
        let me = self.key.public();
        let t = transcript(&TranscriptInput {
            room: &self.room,
            fps: &self.fps,
            host_identity: &host_id,
            host_nonce: &host_nonce,
            password_required,
            member_identity: &me,
            member_name: &self.name,
            member_nonce: &self.nonce,
            spake_a: &self.spake_a,
            spake_b: &self.spake_b,
        });
        self.session_key = session_key(&spake_key, &t);
        self.transcript = Some(t);
        self.state = MemberState::AwaitHostConfirm;
        AuthMsg::MemberConfirm {
            mac: mac(&self.session_key, "member", &t),
            sig: self.key.sign(&sig_msg("member", &t)),
        }
    }

    /// Supplies a password after [`MemberStep::NeedPassword`] or a `BadPassword` reject.
    pub fn submit_password(&mut self, password: String) -> MemberStep {
        if !matches!(
            self.state,
            MemberState::NeedPassword | MemberState::AwaitHostHello
        ) && self.host.is_none()
        {
            return MemberStep::Wait;
        }
        self.password = Some(password).filter(|p| !p.is_empty());
        if self.host.is_none() {
            return MemberStep::Wait;
        }
        self.hello()
    }

    pub fn on_msg(&mut self, msg: AuthMsg) -> MemberStep {
        match (self.state, msg) {
            (
                MemberState::AwaitHostHello,
                AuthMsg::HostHello {
                    version,
                    host_identity,
                    password_required,
                    nonce,
                },
            ) => {
                if version != AUTH_VERSION {
                    return self.fail("auth version");
                }
                if self.expected_host.is_some_and(|e| e != host_identity) {
                    return self.fail("unexpected host identity");
                }
                self.host = Some((host_identity, nonce, password_required));
                self.hello()
            }
            (MemberState::AwaitHostSpake, AuthMsg::HostSpake { spake }) => {
                let Some(s) = self.spake.take() else {
                    return self.fail("no spake state");
                };
                self.spake_b = spake.clone();
                let Ok(key) = s.finish(&spake) else {
                    return self.fail("bad spake message");
                };
                MemberStep::Send(vec![self.confirm(key)])
            }
            (MemberState::AwaitHostConfirm, AuthMsg::HostConfirm { mac: their, sig }) => {
                let (Some(t), Some((host_id, _, _))) = (self.transcript, self.host) else {
                    return self.fail("no transcript");
                };
                if !ct_eq(&mac(&self.session_key, "host", &t), &their)
                    || !verify(&host_id, &sig_msg("host", &t), &sig)
                {
                    return self.fail("host failed to authenticate");
                }
                self.state = MemberState::Done;
                MemberStep::Authenticated {
                    host: host_id,
                    words: words_for(&t),
                    send: Vec::new(),
                }
            }
            (
                MemberState::AwaitHostSpake | MemberState::AwaitHostConfirm,
                AuthMsg::Rejected {
                    reason,
                    retry_after_ms,
                },
            ) => {
                self.spake = None;
                self.state = if retry_after_ms > 0 {
                    MemberState::NeedPassword
                } else {
                    MemberState::Failed
                };
                if reason == AuthReject::BadPassword {
                    self.password = None;
                }
                MemberStep::Rejected {
                    reason,
                    retry_after_ms,
                }
            }
            (MemberState::Done | MemberState::Failed, _) => MemberStep::Wait,
            _ => self.fail("unexpected auth message"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fps() -> Fingerprints {
        Fingerprints {
            member: "SHA-256 AA:01".into(),
            host: "SHA-256 BB:02".into(),
        }
    }

    fn host_cfg(password: Option<&str>) -> HostAuthConfig {
        HostAuthConfig {
            room: "ABCD2345".into(),
            key: IdentityKey::from_secret([1; 32]),
            password: password.map(str::to_owned),
        }
    }

    /// Runs a full handshake. Returns (host step outcome, member outcome).
    fn run(
        cfg: &HostAuthConfig,
        throttle: &mut AuthThrottle,
        member_pw: Option<&str>,
        host_fps: Fingerprints,
        member_fps: Fingerprints,
        now: i64,
    ) -> (Option<HostStep>, Option<MemberStep>) {
        let (mut h, hello) = HostAuth::start(cfg, host_fps, random_nonce());
        let mut m = MemberAuth::new(
            IdentityKey::from_secret([2; 32]),
            "Mia",
            &cfg.room,
            member_pw.map(str::to_owned),
            member_fps,
            None,
        );
        let mut to_host = match m.on_msg(hello) {
            MemberStep::Send(v) => v,
            other => return (None, Some(other)),
        };
        let mut last_host = None;
        let mut last_member = None;
        for _ in 0..4 {
            let mut to_member = Vec::new();
            for msg in to_host.drain(..) {
                let step = h.on_msg(cfg, throttle, now, msg);
                match &step {
                    HostStep::Send(v) => to_member.extend(v.clone()),
                    HostStep::Authenticated { send, .. } | HostStep::Reject { send, .. } => {
                        to_member.extend(send.clone())
                    }
                    HostStep::Wait => {}
                }
                last_host = Some(step);
            }
            for msg in to_member {
                let step = m.on_msg(msg);
                if let MemberStep::Send(v) = &step {
                    to_host.extend(v.clone());
                }
                last_member = Some(step);
            }
            if to_host.is_empty() {
                break;
            }
        }
        (last_host, last_member)
    }

    #[test]
    fn word_list_is_256_distinct_simple_words() {
        let set: std::collections::HashSet<_> = WORDS.iter().collect();
        assert_eq!(set.len(), 256);
        assert!(
            WORDS
                .iter()
                .all(|w| w.len() >= 3 && w.len() <= 8 && w.chars().all(|c| c.is_ascii_lowercase()))
        );
    }

    #[test]
    fn passwordless_room_authenticates_and_both_sides_show_the_same_words() {
        let cfg = host_cfg(None);
        let mut th = AuthThrottle::default();
        let (h, m) = run(&cfg, &mut th, None, fps(), fps(), 0);
        let Some(HostStep::Authenticated {
            words: hw,
            display_name,
            ..
        }) = h
        else {
            panic!("{h:?}")
        };
        let Some(MemberStep::Authenticated {
            words: mw, host, ..
        }) = m
        else {
            panic!("{m:?}")
        };
        assert_eq!(hw, mw);
        assert_eq!(display_name, "Mia");
        assert_eq!(host, cfg.key.public());
    }

    #[test]
    fn man_in_the_middle_changes_the_words() {
        // A MITM terminates DTLS on both sides: each side sees a different peer fingerprint.
        let cfg = host_cfg(None);
        let mut th = AuthThrottle::default();
        let (_, m1) = run(&cfg, &mut th, None, fps(), fps(), 0);
        let host_view = Fingerprints {
            member: "SHA-256 EE:EE".into(),
            host: fps().host,
        };
        // Different fingerprints on each side -> the MACs disagree and the handshake fails.
        let (h2, _) = run(&cfg, &mut th, None, host_view, fps(), 0);
        assert!(
            matches!(h2, Some(HostStep::Reject { close: true, .. })),
            "{h2:?}"
        );
        let Some(MemberStep::Authenticated { words, .. }) = m1 else {
            panic!()
        };
        assert_eq!(words.len(), 4);
    }

    #[test]
    fn correct_password_authenticates() {
        let cfg = host_cfg(Some("hunter2"));
        let mut th = AuthThrottle::default();
        let (h, m) = run(&cfg, &mut th, Some("hunter2"), fps(), fps(), 0);
        assert!(matches!(h, Some(HostStep::Authenticated { .. })), "{h:?}");
        assert!(matches!(m, Some(MemberStep::Authenticated { .. })), "{m:?}");
    }

    #[test]
    fn password_is_bound_to_both_dtls_fingerprints() {
        // Right password, but the host sees a different member fingerprint (relay MITM).
        let cfg = host_cfg(Some("hunter2"));
        let mut th = AuthThrottle::default();
        let host_view = Fingerprints {
            member: "SHA-256 EE:EE".into(),
            host: fps().host,
        };
        let (h, _) = run(&cfg, &mut th, Some("hunter2"), host_view, fps(), 0);
        assert!(matches!(h, Some(HostStep::Reject { .. })), "{h:?}");
    }

    #[test]
    fn missing_password_asks_the_user() {
        let cfg = host_cfg(Some("pw"));
        let mut th = AuthThrottle::default();
        let (_, m) = run(&cfg, &mut th, None, fps(), fps(), 0);
        assert_eq!(m, Some(MemberStep::NeedPassword));
    }

    #[test]
    fn wrong_password_is_rejected_with_growing_delay_then_banned() {
        let cfg = host_cfg(Some("right"));
        let mut th = AuthThrottle::default();
        let mut now = 0;
        let mut delays = Vec::new();
        for _ in 0..5 {
            let (_, m) = run(&cfg, &mut th, Some("wrong"), fps(), fps(), now);
            let Some(MemberStep::Rejected {
                reason: AuthReject::BadPassword,
                retry_after_ms,
            }) = m
            else {
                panic!("{m:?}")
            };
            delays.push(retry_after_ms);
            // Trying again too early is refused without counting as a guess.
            let (_, early) = run(&cfg, &mut th, Some("right"), fps(), fps(), now + 1);
            assert!(
                matches!(
                    early,
                    Some(MemberStep::Rejected {
                        reason: AuthReject::TooManyAttempts,
                        ..
                    })
                ),
                "{early:?}"
            );
            now += retry_after_ms as i64 * 1000;
        }
        assert_eq!(delays, vec![1000, 2000, 4000, 8000, 8000]);
        // The 6th failure bans the identity (connection closed, retry 0).
        let (h, m) = run(&cfg, &mut th, Some("wrong"), fps(), fps(), now);
        assert!(matches!(h, Some(HostStep::Reject { close: true, .. })));
        assert_eq!(
            m,
            Some(MemberStep::Rejected {
                reason: AuthReject::TooManyAttempts,
                retry_after_ms: 0
            })
        );
        // Even the right password is refused while banned.
        let (_, m) = run(&cfg, &mut th, Some("right"), fps(), fps(), now + 1_000_000);
        assert!(matches!(
            m,
            Some(MemberStep::Rejected {
                reason: AuthReject::TooManyAttempts,
                ..
            })
        ));
    }

    #[test]
    fn twenty_failures_a_minute_pause_all_password_attempts() {
        let mut th = AuthThrottle::default();
        let now = 1_000_000;
        for i in 0..GLOBAL_FAILURES {
            th.record_failure(&Identity([i as u8 + 10; 32]), now + i as i64);
        }
        let fresh = Identity([200; 32]);
        assert!(
            matches!(th.check(&fresh, now + 1000), Err((AuthReject::TooManyAttempts, r)) if r > 50_000)
        );
        assert!(th.check(&fresh, now + GLOBAL_PAUSE_US + 100).is_ok());
    }

    #[test]
    fn forged_host_is_detected_by_the_member() {
        // After a host transfer the member pins the new host's identity.
        let cfg = host_cfg(None);
        let (_, hello) = HostAuth::start(&cfg, fps(), random_nonce());
        let mut m = MemberAuth::new(
            IdentityKey::from_secret([2; 32]),
            "Mia",
            &cfg.room,
            None,
            fps(),
            Some(Identity([9; 32])),
        );
        assert_eq!(
            m.on_msg(hello),
            MemberStep::Failed("unexpected host identity")
        );
    }

    #[test]
    fn signatures_are_checked() {
        let key = IdentityKey::from_secret([5; 32]);
        let sig = key.sign(b"hello");
        assert!(verify(&key.public(), b"hello", &sig));
        assert!(!verify(&key.public(), b"hellO", &sig));
        assert!(!verify(&Identity([5; 32]), b"hello", &sig));
        assert!(!verify(&key.public(), b"hello", &sig[..10]));
    }
}
