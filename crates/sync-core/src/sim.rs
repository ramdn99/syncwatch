//! Deterministic simulation harness: SimClock (skewed, drifting local clocks), FakeNet
//! (latency, jitter, loss and reordering on the unreliable channel; reliable messages stay
//! ordered), SimPlayer (load/seek latency, buffering, playback-rate drift) and a [`World`] that
//! wires peers together in a host star. Every message goes through `protocol::encode/decode`,
//! so the simulation also exercises wire validation.

use crate::peer::Peer;
use crate::types::*;
use crate::{HostConfig, Peer as P};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::{BTreeMap, HashMap};
use syncwatch_protocol::*;

const STEP_US: i64 = 2_000;
const TICK_US: i64 = 20_000;
const REPORT_US: i64 = 100_000;

#[derive(Debug, Clone, Copy)]
pub struct LinkConfig {
    pub latency_us: i64,
    pub jitter_us: i64,
    /// Loss probability for the UNRELIABLE channel (heartbeats, clock pings).
    pub loss: f64,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            latency_us: 30_000,
            jitter_us: 5_000,
            loss: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub name: String,
    /// Local monotonic clock = true time * (1 + drift) + offset.
    pub clock_offset_us: i64,
    pub clock_drift_ppm: f64,
    /// Playback runs this much faster/slower than nominal (decoder clock drift).
    pub player_drift_ppm: f64,
    pub link: LinkConfig,
    pub load_latency_us: i64,
    pub seek_latency_us: i64,
    /// Identity of the local copy this node "chooses" when asked; None = missing file.
    pub file: Option<FileIdentity>,
    /// Accept host magnets automatically.
    pub accept_magnets: bool,
    pub tracks: Vec<LocalTrack>,
}

impl NodeConfig {
    pub fn named(name: &str) -> Self {
        Self {
            name: name.into(),
            clock_offset_us: 0,
            clock_drift_ppm: 0.0,
            player_drift_ppm: 0.0,
            link: LinkConfig::default(),
            load_latency_us: 150_000,
            seek_latency_us: 80_000,
            file: None,
            accept_magnets: true,
            tracks: default_tracks(),
        }
    }
}

/// Two audio and three subtitle tracks, like the test MKV.
pub fn default_tracks() -> Vec<LocalTrack> {
    let t = |id, kind, ff, lang: &str, title: Option<&str>, codec: &str| LocalTrack {
        id,
        kind,
        ff_index: Some(ff),
        lang: Some(lang.into()),
        title: title.map(Into::into),
        codec: Some(codec.into()),
        external_id: None,
    };
    vec![
        t(1, TrackKind::Audio, 1, "jpn", None, "eac3"),
        t(2, TrackKind::Audio, 2, "eng", None, "eac3"),
        t(1, TrackKind::Sub, 3, "eng", None, "subrip"),
        t(2, TrackKind::Sub, 4, "eng", Some("Dubtitle"), "subrip"),
        t(3, TrackKind::Sub, 5, "eng", Some("SDH"), "subrip"),
    ]
}

/// A simulated player.
#[derive(Debug, Clone)]
pub struct SimPlayer {
    pub loaded: bool,
    load_at: Option<i64>,
    pub pos_ms: f64,
    pub paused: bool,
    pub speed: u32,
    seek_until: Option<i64>,
    pub drift_ppm: f64,
    pub tracks: Vec<LocalTrack>,
    base_tracks: Vec<LocalTrack>,
    tracks_dirty: bool,
    pub audio: Option<Option<i64>>,
    pub sub: Option<Option<i64>>,
    pub sub_delay: i32,
    pub audio_delay: i32,
    pub duration_ms: i64,
    last_report: Option<i64>,
    pub load_latency_us: i64,
    pub seek_latency_us: i64,
    /// (true time, command) log.
    pub log: Vec<(i64, PlayerCmd)>,
    /// True time when playback last went from paused to playing.
    pub started_at: Vec<i64>,
    pub buffering_until: Option<i64>,
}

impl SimPlayer {
    pub fn new(cfg: &NodeConfig) -> Self {
        Self {
            loaded: false,
            load_at: None,
            pos_ms: 0.0,
            paused: true,
            speed: 1000,
            seek_until: None,
            drift_ppm: cfg.player_drift_ppm,
            tracks: Vec::new(),
            base_tracks: cfg.tracks.clone(),
            tracks_dirty: false,
            audio: None,
            sub: None,
            sub_delay: 0,
            audio_delay: 0,
            duration_ms: 1_708_352,
            last_report: None,
            load_latency_us: cfg.load_latency_us,
            seek_latency_us: cfg.seek_latency_us,
            log: Vec::new(),
            started_at: Vec::new(),
            buffering_until: None,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.loaded && !self.paused && self.seek_until.is_none() && self.buffering_until.is_none()
    }

    pub fn apply(&mut self, now: i64, cmd: PlayerCmd) {
        self.log.push((now, cmd.clone()));
        match cmd {
            PlayerCmd::LoadLocal | PlayerCmd::LoadTorrentStream { .. } => {
                self.loaded = false;
                self.load_at = Some(now + self.load_latency_us);
            }
            PlayerCmd::Stop => {
                self.loaded = false;
                self.load_at = None;
                self.tracks.clear();
            }
            PlayerCmd::SetPaused(p) => {
                if self.paused && !p {
                    self.started_at.push(now);
                }
                self.paused = p;
            }
            PlayerCmd::Seek(t) => {
                self.pos_ms = t.clamp(0, self.duration_ms) as f64;
                self.seek_until = Some(now + self.seek_latency_us);
            }
            PlayerCmd::SetSpeed(s) => self.speed = s,
            PlayerCmd::SelectAudio(v) => self.audio = Some(v),
            PlayerCmd::SelectSub(v) => self.sub = Some(v),
            PlayerCmd::SubDelay(d) => self.sub_delay = d,
            PlayerCmd::AudioDelay(d) => self.audio_delay = d,
            PlayerCmd::AddExternal { external_id, kind } => {
                let kind = match kind {
                    ExternalKind::Audio => TrackKind::Audio,
                    ExternalKind::Subtitle => TrackKind::Sub,
                };
                let id = self
                    .tracks
                    .iter()
                    .filter(|t| t.kind == kind)
                    .map(|t| t.id)
                    .max()
                    .unwrap_or(0)
                    + 1;
                self.tracks.push(LocalTrack {
                    id,
                    kind,
                    ff_index: None,
                    lang: None,
                    title: None,
                    codec: None,
                    external_id: Some(external_id),
                });
                self.tracks_dirty = true;
            }
        }
    }

    pub fn step(&mut self, now: i64, dt_us: i64) -> Vec<PlayerObs> {
        let mut obs = Vec::new();
        if let Some(t) = self.load_at
            && now >= t
        {
            self.load_at = None;
            self.loaded = true;
            self.pos_ms = 0.0;
            self.paused = true;
            self.tracks = self.base_tracks.clone();
            obs.push(PlayerObs::FileLoaded {
                tracks: self.tracks.clone(),
                duration_ms: Some(self.duration_ms),
            });
        }
        if !self.loaded {
            return obs;
        }
        if self.tracks_dirty {
            self.tracks_dirty = false;
            obs.push(PlayerObs::Tracks(self.tracks.clone()));
        }
        if let Some(t) = self.seek_until
            && now >= t
        {
            self.seek_until = None;
            obs.push(PlayerObs::SeekDone);
        }
        if self.buffering_until.is_some_and(|t| now >= t) {
            self.buffering_until = None;
        }
        if self.is_playing() {
            let adv =
                dt_us as f64 / 1000.0 * self.speed as f64 / 1000.0 * (1.0 + self.drift_ppm / 1e6);
            self.pos_ms = (self.pos_ms + adv).min(self.duration_ms as f64);
        }
        if self.last_report.is_none_or(|t| now - t >= REPORT_US) {
            self.last_report = Some(now);
            obs.push(PlayerObs::Position {
                position_ms: self.pos_ms.round() as i64,
                paused: self.paused,
                buffering: self.seek_until.is_some() || self.buffering_until.is_some(),
            });
        }
        obs
    }
}

pub struct Node {
    pub peer: Peer,
    pub id: MemberId,
    pub identity: Identity,
    pub cfg: NodeConfig,
    pub player: SimPlayer,
    pub ui: Vec<(i64, UiEvent)>,
    pub violations: Vec<&'static str>,
    pub sent: Vec<(i64, Message)>,
    pub effects: Vec<(i64, Effect)>,
    pub connected: bool,
    /// External files: host side = sources, member side = received bytes.
    pub ext_files: HashMap<ExternalId, Vec<u8>>,
    pub ext_complete: Vec<ExternalId>,
    /// Torrent metadata received from the host (infohash, bytes).
    pub meta_received: Vec<(Infohash, Vec<u8>)>,
    next_tick: i64,
    pending_events: Vec<(i64, Event)>,
}

impl Node {
    pub fn ui_events(&self) -> impl Iterator<Item = &UiEvent> {
        self.ui.iter().map(|(_, u)| u)
    }
}

struct InFlight {
    at: i64,
    seq: u64,
    from: usize,
    to: usize,
    frame: Vec<u8>,
}

struct Upload {
    to: usize,
    external_id: ExternalId,
    offset: u64,
}

pub struct World {
    pub now: i64,
    pub nodes: Vec<Node>,
    net: Vec<InFlight>,
    seq: u64,
    last_reliable: HashMap<(usize, usize), i64>,
    rng: ChaCha8Rng,
    uploads: Vec<Upload>,
    /// (member node, file) the host does not send (like the app's "skip relayed members").
    pub withhold: Vec<(usize, ExternalId)>,
    pub wall_base_ms: i64,
    /// Transports closing after their queued messages are delivered: (true time, node).
    closing: Vec<(i64, usize)>,
    /// Count of messages per kind (for traffic assertions).
    pub traffic: BTreeMap<String, usize>,
}

impl World {
    pub fn new(seed: u64, host_cfg: NodeConfig) -> Self {
        let identity = Identity([1; 32]);
        let peer = P::host(
            HostConfig::new(identity, &host_cfg.name),
            host_cfg.clock_offset_us,
        );
        let mut w = Self {
            now: 0,
            nodes: Vec::new(),
            net: Vec::new(),
            seq: 0,
            last_reliable: HashMap::new(),
            rng: ChaCha8Rng::seed_from_u64(seed),
            uploads: Vec::new(),
            withhold: Vec::new(),
            wall_base_ms: 1_800_000_000_000,
            closing: Vec::new(),
            traffic: BTreeMap::new(),
        };
        w.push_node(peer, MemberId::HOST, identity, host_cfg);
        w
    }

    fn push_node(
        &mut self,
        peer: Peer,
        id: MemberId,
        identity: Identity,
        cfg: NodeConfig,
    ) -> usize {
        let player = SimPlayer::new(&cfg);
        self.nodes.push(Node {
            peer,
            id,
            identity,
            cfg,
            player,
            ui: Vec::new(),
            violations: Vec::new(),
            sent: Vec::new(),
            effects: Vec::new(),
            connected: true,
            ext_files: HashMap::new(),
            ext_complete: Vec::new(),
            meta_received: Vec::new(),
            next_tick: 0,
            pending_events: Vec::new(),
        });
        self.nodes.len() - 1
    }

    pub fn host_idx(&self) -> usize {
        self.nodes
            .iter()
            .position(|n| n.peer.is_host())
            .unwrap_or(0)
    }

    /// A member joins (handshake assumed done; Milestone 5 adds authentication).
    pub fn add_member(&mut self, cfg: NodeConfig) -> usize {
        let n = self.nodes.len() as u32;
        let identity = Identity([n as u8 + 1; 32]);
        let peer = P::member(identity, &cfg.name);
        let idx = self.push_node(peer, MemberId(n), identity, cfg);
        let name = self.nodes[idx].cfg.name.clone();
        let h = self.host_idx();
        self.deliver_event(
            h,
            Event::PeerJoined {
                member: MemberId(n),
                identity,
                display_name: name,
            },
        );
        idx
    }

    pub fn disconnect(&mut self, idx: usize) {
        self.nodes[idx].connected = false;
        let member = self.nodes[idx].id;
        let h = self.host_idx();
        self.deliver_event(h, Event::PeerLeft { member });
    }

    pub fn clock(&self, idx: usize) -> Clock {
        let c = &self.nodes[idx].cfg;
        let mono = (self.now as f64 * (1.0 + c.clock_drift_ppm / 1e6)) as i64 + c.clock_offset_us;
        Clock {
            mono_us: mono,
            wall_ms: self.wall_base_ms + self.now / 1000,
        }
    }

    pub fn ui(&mut self, idx: usize, a: UiAction) {
        self.deliver_event(idx, Event::Ui(a));
    }

    pub fn deliver_event(&mut self, idx: usize, ev: Event) {
        let clock = self.clock(idx);
        let node = &mut self.nodes[idx];
        let (identity, name) = (node.identity, node.cfg.name.clone());
        let effects = node.peer.handle(clock, ev, identity, &name);
        self.dispatch(idx, effects);
    }

    /// Schedule an event for a node at a later true time.
    pub fn schedule(&mut self, idx: usize, delay_us: i64, ev: Event) {
        let at = self.now + delay_us;
        self.nodes[idx].pending_events.push((at, ev));
    }

    /// Host registers an external file it holds (validated, hashed) and adds it to the room.
    pub fn host_add_external(
        &mut self,
        kind: ExternalKind,
        name: &str,
        format: &str,
        bytes: Vec<u8>,
    ) -> ExternalId {
        let h = self.host_idx();
        let blake3 = *blake3::hash(&bytes).as_bytes();
        let before = match &self.nodes[h].peer {
            Peer::Host(host) => host.tracks().manifest.len(),
            Peer::Member(_) => 0,
        };
        self.ui(
            h,
            UiAction::AddExternal {
                kind,
                display_name: name.into(),
                format: format.into(),
                size: bytes.len() as u64,
                blake3,
            },
        );
        let id = match &self.nodes[h].peer {
            Peer::Host(host) if host.tracks().manifest.len() > before => {
                host.tracks().manifest.last().map(|e| e.external_id)
            }
            _ => None,
        }
        .unwrap_or(ExternalId(0));
        self.nodes[h].ext_files.insert(id, bytes);
        id
    }

    fn send(&mut self, from: usize, to: usize, msg: &Message) {
        if !self.nodes[to].connected || !self.nodes[from].connected {
            return;
        }
        let frame = match encode(msg) {
            Ok(f) => f,
            Err(e) => panic!("sim: message failed validation: {e:?} {msg:?}"),
        };
        *self.traffic.entry(format!("{:?}", msg.kind())).or_default() += 1;
        self.nodes[from].sent.push((self.now, msg.clone()));
        let link = if self.nodes[from].peer.is_host() {
            self.nodes[to].cfg.link
        } else {
            self.nodes[from].cfg.link
        };
        let jitter = if link.jitter_us > 0 {
            self.rng.random_range(0..=link.jitter_us)
        } else {
            0
        };
        let mut at = self.now + link.latency_us + jitter;
        match msg.channel() {
            Channel::Unreliable => {
                if link.loss > 0.0 && self.rng.random_bool(link.loss.clamp(0.0, 1.0)) {
                    return;
                }
            }
            Channel::Reliable => {
                let last = self.last_reliable.entry((from, to)).or_insert(i64::MIN);
                at = at.max(*last);
                *last = at;
            }
        }
        self.seq += 1;
        self.net.push(InFlight {
            at,
            seq: self.seq,
            from,
            to,
            frame,
        });
    }

    fn idx_of(&self, m: MemberId) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == m)
    }

    fn dispatch(&mut self, idx: usize, effects: Vec<Effect>) {
        for e in effects {
            self.nodes[idx].effects.push((self.now, e.clone()));
            match e {
                Effect::SendToHost(m) => {
                    let h = self.host_idx();
                    if h != idx {
                        self.send(idx, h, &m);
                    }
                }
                Effect::SendTo(to, m) => {
                    if let Some(t) = self.idx_of(to) {
                        self.send(idx, t, &m);
                    }
                }
                Effect::Broadcast(m) => {
                    for t in 0..self.nodes.len() {
                        if t != idx && !self.nodes[t].peer.is_host() {
                            self.send(idx, t, &m);
                        }
                    }
                }
                Effect::Player(c) => self.nodes[idx].player.apply(self.now, c),
                Effect::Ui(u) => {
                    self.auto_respond(idx, &u);
                    self.nodes[idx].ui.push((self.now, u));
                }
                Effect::Disconnect(m) => {
                    // Flush first: close once everything already queued for it is delivered.
                    if let Some(t) = self.idx_of(m) {
                        let last = self
                            .net
                            .iter()
                            .filter(|f| f.to == t)
                            .map(|f| f.at)
                            .max()
                            .unwrap_or(self.now);
                        self.closing.push((last + 1, t));
                    }
                }
                Effect::ProtocolViolation { what, .. } => self.nodes[idx].violations.push(what),
                Effect::WriteExternalChunk {
                    external_id,
                    offset,
                    data,
                } => {
                    let f = self.nodes[idx].ext_files.entry(external_id).or_default();
                    assert_eq!(f.len() as u64, offset, "chunks must be written in order");
                    f.extend_from_slice(&data);
                }
                Effect::ExternalComplete { external_id } => {
                    self.nodes[idx].ext_complete.push(external_id)
                }
                Effect::DiscardExternal { external_id } => {
                    self.nodes[idx].ext_files.remove(&external_id);
                }
                Effect::StartUpload {
                    to,
                    external_id,
                    offset,
                } => {
                    if let Some(t) = self.idx_of(to)
                        && self.withhold.contains(&(t, external_id))
                    {
                        self.send(idx, t, &Message::FileWithheld { external_id });
                    } else if let Some(t) = self.idx_of(to) {
                        self.uploads.push(Upload {
                            to: t,
                            external_id,
                            offset,
                        });
                    }
                }
                Effect::CancelUpload { to, external_id } => {
                    if let Some(t) = self.idx_of(to) {
                        self.uploads
                            .retain(|u| !(u.to == t && u.external_id == external_id));
                    }
                }
                Effect::StartTorrent { .. }
                | Effect::StopTorrent { .. }
                | Effect::TorrentTarget { .. } => {}
                Effect::TorrentMetaReceived {
                    infohash, torrent, ..
                } => self.nodes[idx].meta_received.push((infohash, torrent)),
            }
        }
    }

    /// Scripted user behavior: choose the local file when asked, accept magnets.
    fn auto_respond(&mut self, idx: usize, u: &UiEvent) {
        match u {
            UiEvent::ShowMissingFilePanel { .. } => {
                if let Some(id) = self.nodes[idx].cfg.file {
                    self.schedule(idx, 100_000, Event::LocalFile(LocalFileEvent::Verifying));
                    self.schedule(
                        idx,
                        400_000,
                        Event::LocalFile(LocalFileEvent::Identified(id)),
                    );
                }
            }
            UiEvent::ConfirmMagnet { .. } if self.nodes[idx].cfg.accept_magnets => {
                self.schedule(idx, 200_000, Event::Ui(UiAction::AcceptMagnet));
            }
            _ => {}
        }
    }

    pub fn step(&mut self) {
        self.now += STEP_US;

        // Network deliveries due now, in (time, seq) order.
        self.net.sort_by_key(|m| (m.at, m.seq));
        let due: Vec<InFlight> = {
            let split = self
                .net
                .iter()
                .position(|m| m.at > self.now)
                .unwrap_or(self.net.len());
            self.net.drain(..split).collect()
        };
        for m in due {
            if !self.nodes[m.to].connected {
                continue;
            }
            let msg = match decode(&m.frame) {
                Ok(x) => x,
                Err(e) => panic!("sim: decode failed {e:?}"),
            };
            let from = Sender {
                member: self.nodes[m.from].id,
                identity: self.nodes[m.from].identity,
            };
            self.deliver_event(m.to, Event::Net { from, msg });
        }
        // Transports that were asked to close do so once their queued messages are delivered.
        let now = self.now;
        let mut closed = Vec::new();
        self.closing.retain(|(at, idx)| {
            if *at <= now {
                closed.push(*idx);
                false
            } else {
                true
            }
        });
        for idx in closed {
            self.nodes[idx].connected = false;
        }
        // Uploads: one chunk per step per upload (no artificial limit beyond the sim step).
        let mut i = 0;
        while i < self.uploads.len() {
            let h = self.host_idx();
            let (to, id, off) = (
                self.uploads[i].to,
                self.uploads[i].external_id,
                self.uploads[i].offset,
            );
            let Some(src) = self.nodes[h].ext_files.get(&id).cloned() else {
                self.uploads.remove(i);
                continue;
            };
            if off as usize >= src.len() {
                self.uploads.remove(i);
                continue;
            }
            let end = (off as usize + limits::CHUNK_BYTES).min(src.len());
            let msg = Message::FileChunk {
                external_id: id,
                offset: off,
                data: src[off as usize..end].to_vec(),
            };
            self.send(h, to, &msg);
            self.uploads[i].offset = end as u64;
            i += 1;
        }
        for idx in 0..self.nodes.len() {
            // Scheduled user/app events.
            let mut due_ev = Vec::new();
            self.nodes[idx].pending_events.retain(|(at, ev)| {
                if *at <= self.now {
                    due_ev.push(ev.clone());
                    false
                } else {
                    true
                }
            });
            for ev in due_ev {
                self.deliver_event(idx, ev);
            }
            let obs = self.nodes[idx].player.step(self.now, STEP_US);
            for o in obs {
                self.deliver_event(idx, Event::Player(o));
            }
            let mono = self.clock(idx).mono_us;
            let deadline_due = self.nodes[idx]
                .peer
                .next_deadline()
                .is_some_and(|d| mono >= d);
            if self.now >= self.nodes[idx].next_tick || deadline_due {
                self.nodes[idx].next_tick = self.now + TICK_US;
                self.deliver_event(idx, Event::Tick);
            }
        }
    }

    pub fn run_for(&mut self, us: i64) {
        let end = self.now + us;
        while self.now < end {
            self.step();
        }
    }

    pub fn pos(&self, idx: usize) -> f64 {
        self.nodes[idx].player.pos_ms
    }

    /// Largest |position - host position| among connected, loaded, playing members.
    pub fn max_divergence_ms(&self) -> f64 {
        let h = self.host_idx();
        let hp = self.pos(h);
        self.nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| *i != h && n.connected && n.player.loaded)
            .map(|(_, n)| (n.player.pos_ms - hp).abs())
            .fold(0.0, f64::max)
    }

    pub fn sample_identity() -> FileIdentity {
        FileIdentity {
            sample_blake3: [42; 32],
            size: 1_288_181_962,
            duration_ms: Some(1_708_352),
        }
    }

    pub fn local_descriptor() -> MediaDescriptor {
        MediaDescriptor::LocalFile(LocalMedia {
            name: "test.mkv".into(),
            identity: Self::sample_identity(),
        })
    }
}
