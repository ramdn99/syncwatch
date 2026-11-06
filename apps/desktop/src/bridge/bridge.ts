// The hidden WebView2 "bridge" (spec section 1 "Transport"). WebRTC plumbing ONLY: it owns
// RTCPeerConnections and data channels and forwards bytes. It has no timers and no timing logic;
// keepalives, the watchdog, clock sync and scheduling all live in Rust.
//
// IPC is binary in both directions (no base64):
//   Rust -> bridge: a Tauri Channel delivering ArrayBuffers
//   bridge -> Rust: invoke('bridge_in', Uint8Array)
//   frames: [0x01][peer u32 LE][lane u8][payload]  or  [0x02][UTF-8 JSON]

import { Channel, invoke } from '@tauri-apps/api/core';

const OP_DATA = 1;
const OP_JSON = 2;
const OP_BATCH = 3;
const MAX_BATCH = 1 << 20;
// A drained channel reports fresh credit to Rust (bufferedamountlow). This MUST be well below
// Rust's bulk window (bridge.rs BULK_HIGH_WATER), or the event never fires and transfers stall.
const LOW_WATER = 64 * 1024;
// The in-page benchmark keeps its own, larger buffer.
const BENCH_WATER = 1 << 20;

type SignalData =
  | { kind: 'offer'; sdp: string }
  | { kind: 'answer'; sdp: string }
  | { kind: 'candidate'; candidate: string; sdpMid: string | null; sdpMLineIndex: number | null }
  | { kind: 'endOfCandidates' };

interface IceConfig {
  iceServers: RTCIceServer[];
  relayOnly: boolean;
}

type Command =
  | { cmd: 'createPeer'; peer: number; offerer: boolean; ice: IceConfig }
  | { cmd: 'remoteSignal'; peer: number; data: SignalData }
  | { cmd: 'closePeer'; peer: number }
  | { cmd: 'ping'; seq: number }
  | { cmd: 'devDropTraffic'; peer: number }
  | { cmd: 'devFreeze' }
  | { cmd: 'devBench'; bytes: number; chunk: number };

interface Peer {
  pc: RTCPeerConnection;
  lanes: [RTCDataChannel, RTCDataChannel, RTCDataChannel];
  pending: RTCIceCandidateInit[];
  remoteSet: boolean;
  opened: boolean;
  dropped: boolean;
  /** Cumulative bytes of DATA frames received from Rust (flow-control credit). */
  rx: number;
}

const peers = new Map<number, Peer>();
const enc = new TextEncoder();
const dec = new TextDecoder();

// Outgoing frames are queued; one invoke is in flight at a time and carries everything that
// queued up meanwhile (backpressure batching, no timers).
const outQueue: Uint8Array[] = [];
let inFlight = false;

function flush() {
  if (inFlight || outQueue.length === 0) return;
  let size = 0;
  let n = 0;
  while (n < outQueue.length && size < MAX_BATCH) size += outQueue[n++]!.length + 4;
  const frames = outQueue.splice(0, n);
  let msg: Uint8Array;
  if (frames.length === 1) {
    msg = frames[0]!;
  } else {
    msg = new Uint8Array(size + 1);
    const view = new DataView(msg.buffer);
    msg[0] = OP_BATCH;
    let at = 1;
    for (const f of frames) {
      view.setUint32(at, f.length, true);
      msg.set(f, at + 4);
      at += f.length + 4;
    }
  }
  inFlight = true;
  invoke('bridge_in', msg)
    .catch(() => undefined)
    .finally(() => {
      inFlight = false;
      flush();
    });
}

function sendJson(ev: Record<string, unknown>) {
  const body = enc.encode(JSON.stringify(ev));
  const frame = new Uint8Array(body.length + 1);
  frame[0] = OP_JSON;
  frame.set(body, 1);
  outQueue.push(frame);
  flush();
}

function sendData(peer: number, lane: number, payload: ArrayBuffer) {
  const frame = new Uint8Array(payload.byteLength + 6);
  frame[0] = OP_DATA;
  new DataView(frame.buffer).setUint32(1, peer, true);
  frame[5] = lane;
  frame.set(new Uint8Array(payload), 6);
  outQueue.push(frame);
  flush();
}

function reportCredit(id: number, p: Peer) {
  sendJson({ ev: 'buffered', peer: id, rx: p.rx, buffered: p.lanes[0].bufferedAmount + p.lanes[1].bufferedAmount + p.lanes[2].bufferedAmount });
}

function closePeer(id: number, reason: string, notify = true) {
  const p = peers.get(id);
  if (!p) return;
  peers.delete(id);
  try {
    p.pc.close();
  } catch {
    /* already closed */
  }
  if (notify) sendJson({ ev: 'closed', peer: id, reason });
}

async function maybeOpen(id: number) {
  const p = peers.get(id);
  if (!p || p.opened || !p.lanes.every((c) => c.readyState === 'open')) return;
  p.opened = true;
  let relayed = false;
  try {
    const stats = await p.pc.getStats();
    stats.forEach((s) => {
      if (s.type === 'transport' && s.selectedCandidatePairId) {
        const pair = stats.get(s.selectedCandidatePairId);
        const local = pair && stats.get(pair.localCandidateId);
        const remote = pair && stats.get(pair.remoteCandidateId);
        relayed = local?.candidateType === 'relay' || remote?.candidateType === 'relay';
      }
    });
  } catch {
    /* stats unavailable */
  }
  sendJson({
    ev: 'open',
    peer: id,
    relayed,
    localSdp: p.pc.localDescription?.sdp ?? '',
    remoteSdp: p.pc.remoteDescription?.sdp ?? '',
  });
}

function createPeer(id: number, offerer: boolean, ice: IceConfig) {
  closePeer(id, 'replaced', false); // Rust already replaced its state
  const pc = new RTCPeerConnection({ iceServers: ice.iceServers, iceTransportPolicy: ice.relayOnly ? 'relay' : 'all' });
  const reliable = pc.createDataChannel('r', { negotiated: true, id: 0, ordered: true });
  const unreliable = pc.createDataChannel('u', { negotiated: true, id: 1, ordered: false, maxRetransmits: 0 });
  // External-file chunks: their own SCTP stream, so they never delay control messages.
  const bulk = pc.createDataChannel('b', { negotiated: true, id: 2, ordered: true });
  const p: Peer = { pc, lanes: [reliable, unreliable, bulk], pending: [], remoteSet: false, opened: false, dropped: false, rx: 0 };
  peers.set(id, p);
  p.lanes.forEach((ch, lane) => {
    ch.binaryType = 'arraybuffer';
    ch.bufferedAmountLowThreshold = LOW_WATER;
    ch.onopen = () => void maybeOpen(id);
    ch.onmessage = (e) => {
      if (!p.dropped && e.data instanceof ArrayBuffer) sendData(id, lane, e.data);
    };
    ch.onbufferedamountlow = () => reportCredit(id, p);
    ch.onclose = () => {
      if (peers.get(id) === p) closePeer(id, 'channel closed');
    };
  });
  // Trickle ICE: every candidate goes out as soon as it is found.
  pc.onicecandidate = (e) => {
    if (e.candidate) {
      sendJson({ ev: 'localSignal', peer: id, data: { kind: 'candidate', candidate: e.candidate.candidate, sdpMid: e.candidate.sdpMid, sdpMLineIndex: e.candidate.sdpMLineIndex } });
    } else {
      sendJson({ ev: 'localSignal', peer: id, data: { kind: 'endOfCandidates' } });
    }
  };
  const state = () => {
    if (peers.get(id) === p) sendJson({ ev: 'state', peer: id, ice: pc.iceConnectionState, connection: pc.connectionState });
  };
  pc.oniceconnectionstatechange = state;
  pc.onconnectionstatechange = () => {
    state();
    if (pc.connectionState === 'failed' && peers.get(id) === p) closePeer(id, 'connection failed');
  };
  if (offerer) {
    void (async () => {
      try {
        await pc.setLocalDescription(await pc.createOffer());
        sendJson({ ev: 'localSignal', peer: id, data: { kind: 'offer', sdp: pc.localDescription?.sdp ?? '' } });
      } catch (e) {
        sendJson({ ev: 'error', peer: id, message: String(e) });
      }
    })();
  }
}

async function remoteSignal(id: number, data: SignalData) {
  const p = peers.get(id);
  if (!p) return;
  try {
    if (data.kind === 'offer' || data.kind === 'answer') {
      await p.pc.setRemoteDescription({ type: data.kind, sdp: data.sdp });
      p.remoteSet = true;
      for (const c of p.pending.splice(0)) await p.pc.addIceCandidate(c);
      if (data.kind === 'offer') {
        await p.pc.setLocalDescription(await p.pc.createAnswer());
        sendJson({ ev: 'localSignal', peer: id, data: { kind: 'answer', sdp: p.pc.localDescription?.sdp ?? '' } });
      }
    } else if (data.kind === 'candidate') {
      const c: RTCIceCandidateInit = { candidate: data.candidate, sdpMid: data.sdpMid ?? undefined, sdpMLineIndex: data.sdpMLineIndex ?? undefined };
      if (p.remoteSet) await p.pc.addIceCandidate(c);
      else p.pending.push(c);
    }
  } catch (e) {
    sendJson({ ev: 'error', peer: id, message: String(e) });
  }
}

// Dev only: raw data-channel throughput inside this page (no IPC). Rust times it.
async function devBench(total: number, chunk: number) {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  a.onicecandidate = (e) => void (e.candidate && b.addIceCandidate(e.candidate));
  b.onicecandidate = (e) => void (e.candidate && a.addIceCandidate(e.candidate));
  const tx = a.createDataChannel('bench', { negotiated: true, id: 0 });
  const rx = b.createDataChannel('bench', { negotiated: true, id: 0 });
  const buf = new Uint8Array(chunk);
  let sent = 0;
  let got = 0;
  tx.bufferedAmountLowThreshold = BENCH_WATER;
  const pump = () => {
    while (sent < total && tx.bufferedAmount < 4 * BENCH_WATER) {
      tx.send(buf);
      sent += chunk;
    }
  };
  tx.onopen = pump;
  tx.onbufferedamountlow = pump;
  rx.onmessage = (e: MessageEvent<ArrayBuffer>) => {
    got += e.data.byteLength;
    if (got >= total) {
      sendJson({ ev: 'benchDone', bytes: got });
      a.close();
      b.close();
    }
  };
  rx.binaryType = 'arraybuffer';
  await a.setLocalDescription(await a.createOffer());
  await b.setRemoteDescription(a.localDescription!);
  await b.setLocalDescription(await b.createAnswer());
  await a.setRemoteDescription(b.localDescription!);
}

function onCommand(c: Command) {
  switch (c.cmd) {
    case 'ping':
      sendJson({ ev: 'pong', seq: c.seq });
      break;
    case 'createPeer':
      createPeer(c.peer, c.offerer, c.ice);
      break;
    case 'remoteSignal':
      void remoteSignal(c.peer, c.data);
      break;
    case 'closePeer':
      closePeer(c.peer, 'closed by app');
      break;
    case 'devDropTraffic': {
      const p = peers.get(c.peer);
      if (p) p.dropped = true;
      break;
    }
    case 'devBench':
      void devBench(c.bytes, c.chunk);
      break;
    case 'devFreeze':
      // Simulates a hung page: Rust must notice (no pongs) and recreate the bridge.
      for (;;) {
        /* spin */
      }
  }
}

function handleFrame(bytes: Uint8Array<ArrayBuffer>, touched: Set<number>) {
  if (bytes[0] === OP_DATA && bytes.length >= 6) {
    const peer = new DataView(bytes.buffer, bytes.byteOffset).getUint32(1, true);
    const p = peers.get(peer);
    if (!p) return;
    p.rx += bytes.length;
    touched.add(peer);
    const ch = p.lanes[bytes[5]!];
    if (ch && ch.readyState === 'open' && !p.dropped) {
      try {
        ch.send(bytes.subarray(6));
      } catch {
        /* channel closing */
      }
    }
  } else if (bytes[0] === OP_JSON) {
    try {
      onCommand(JSON.parse(dec.decode(bytes.subarray(1))) as Command);
    } catch (e) {
      sendJson({ ev: 'error', peer: null, message: String(e) });
    }
  }
}

const channel = new Channel<ArrayBuffer>();
channel.onmessage = (buf) => {
  const bytes = new Uint8Array(buf);
  const touched = new Set<number>();
  if (bytes[0] === OP_BATCH) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    let at = 1;
    while (at + 4 <= bytes.length) {
      const n = view.getUint32(at, true);
      handleFrame(bytes.subarray(at + 4, at + 4 + n), touched);
      at += 4 + n;
    }
  } else {
    handleFrame(bytes, touched);
  }
  // One credit report per peer per message from Rust.
  for (const id of touched) {
    const p = peers.get(id);
    if (p) reportCredit(id, p);
  }
};
void invoke('bridge_attach', { channel }).then(() => sendJson({ ev: 'ready' }));
