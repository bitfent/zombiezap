//! Network client: one WebSocket to the game server, poll-based (no async
//! coupling with Bevy), identical API on native and wasm via ewebsock.

use bevy::prelude::Resource;
use zz_core::protocol::{BIN_VOICE, ClientMsg, ServerMsg, decode_voice};
use zz_core::snapshot::{Snapshot, SnapshotDecoder};

/// What a frame's worth of polling yields.
#[allow(dead_code)] // consumed by the M3 session wiring
pub enum NetEvent {
    Connected,
    Msg(ServerMsg),
    Snap(Snapshot),
    /// Proximity voice: authoritative speaker slot + 16 kHz mono i16 LE PCM.
    Voice { slot: u8, pcm: Vec<u8> },
    Closed(String),
}

#[derive(Resource)]
pub struct NetClient {
    sender: Option<ewebsock::WsSender>,
    // Mutex only for Sync (Bevy Resource bound); there is exactly one consumer.
    receiver: Option<std::sync::Mutex<ewebsock::WsReceiver>>,
    decoder: SnapshotDecoder,
    connected: bool,
    /// Headless / unit-test inject queue (drained first each poll).
    inject: Vec<NetEvent>,
    /// Binary frames handed to `send_bin` (real socket and/or test capture).
    outbound_bin: Vec<Vec<u8>>,
}

// ewebsock's wasm WsSender holds `Rc<WebSocket>`, which is !Send/!Sync.
// Bevy still requires Resource: Send + Sync. On wasm the app is single-
// threaded (main browser thread only), so this is sound in practice.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for NetClient {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for NetClient {}

#[allow(dead_code)] // consumed by the M3 session wiring
impl NetClient {
    pub fn disconnected() -> Self {
        NetClient {
            sender: None,
            receiver: None,
            decoder: SnapshotDecoder::new(),
            connected: false,
            inject: Vec::new(),
            outbound_bin: Vec::new(),
        }
    }

    pub fn connect(&mut self, url: &str) -> Result<(), String> {
        let (sender, receiver) =
            ewebsock::connect(url, ewebsock::Options::default()).map_err(|e| e.to_string())?;
        self.sender = Some(sender);
        self.receiver = Some(std::sync::Mutex::new(receiver));
        self.decoder = SnapshotDecoder::new(); // fresh stream = fresh baseline
        self.connected = false;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn send_msg(&mut self, msg: &ClientMsg) {
        if let (Some(s), Ok(json)) = (self.sender.as_mut(), serde_json::to_string(msg)) {
            s.send(ewebsock::WsMessage::Text(json));
        }
    }

    pub fn send_bin(&mut self, frame: Vec<u8>) {
        if let Some(s) = self.sender.as_mut() {
            s.send(ewebsock::WsMessage::Binary(frame.clone()));
        }
        // Always mirror into the test capture buffer. Headless match_flow
        // injects GameStart/Snap without owning the socket; if a local server
        // happens to accept `connect_on_start`, frames still need to be
        // observable via `take_outbound_bin`. Cap so production never grows.
        const CAP: usize = 64;
        if self.outbound_bin.len() >= CAP {
            self.outbound_bin.remove(0);
        }
        self.outbound_bin.push(frame);
    }

    /// Drain outbound binary frames (inputs / voice) for tests.
    pub fn take_outbound_bin(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outbound_bin)
    }

    /// Queue a synthetic event (GameStart, Snap, …) for the next `drain`.
    /// Used by headless match-flow tests that have no real socket.
    pub fn inject(&mut self, ev: NetEvent) {
        self.inject.push(ev);
    }

    /// Reset the snapshot decoder baseline. Prefer letting `drain` do this
    /// when it sees `GameStart` **before** decoding later binary frames in the
    /// same batch — calling this *after* those frames were decoded drops the
    /// room's opening keyframe and leaves the client without a baseline until
    /// the next periodic keyframe (~2 s).
    pub fn reset_decoder(&mut self) {
        self.decoder = SnapshotDecoder::new();
    }

    /// Drain everything that arrived since last frame. Call once per frame.
    pub fn drain(&mut self) -> Vec<NetEvent> {
        let mut out = std::mem::take(&mut self.inject);
        let mut events = Vec::new();
        if let Some(r) = self.receiver.as_ref() {
            let r = r.lock().expect("net receiver lock");
            while let Some(ev) = r.try_recv() {
                events.push(ev);
            }
        }
        for ev in events {
            match ev {
                ewebsock::WsEvent::Opened => {
                    self.connected = true;
                    out.push(NetEvent::Connected);
                }
                ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(t)) => {
                    match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(ServerMsg::Ping { t }) => {
                            // answer heartbeats right here — presence is not
                            // gameplay's problem
                            self.send_msg(&ClientMsg::Pong { t });
                        }
                        Ok(m) => {
                            // GameStart must reset the decoder *before* any
                            // snapshot frames that follow in this same drain
                            // batch. The new room's encoder starts with a
                            // keyframe; applying it against a previous room's
                            // baseline (or wiping the baseline *after* the
                            // keyframe was already decoded) desyncs the stream
                            // for up to KEYFRAME_EVERY ticks.
                            if matches!(m, ServerMsg::GameStart { .. }) {
                                self.decoder = SnapshotDecoder::new();
                            }
                            out.push(NetEvent::Msg(m));
                        }
                        Err(_) => {} // unknown control message: drop
                    }
                }
                ewebsock::WsEvent::Message(ewebsock::WsMessage::Binary(b)) => {
                    if b.first() == Some(&BIN_VOICE) {
                        if let Some((slot, pcm)) = decode_voice(&b) {
                            out.push(NetEvent::Voice {
                                slot,
                                pcm: pcm.to_vec(),
                            });
                        }
                    } else if let Ok(snap) = self.decoder.decode(&b) {
                        out.push(NetEvent::Snap(snap));
                    }
                }
                ewebsock::WsEvent::Message(_) => {}
                ewebsock::WsEvent::Error(e) => {
                    self.connected = false;
                    self.sender = None;
                    self.receiver = None;
                    out.push(NetEvent::Closed(e.to_string()));
                    break;
                }
                ewebsock::WsEvent::Closed => {
                    self.connected = false;
                    self.sender = None;
                    self.receiver = None;
                    out.push(NetEvent::Closed("closed".into()));
                    break;
                }
            }
        }
        out
    }
}
