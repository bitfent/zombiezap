//! Network client: one WebSocket to the game server, poll-based (no async
//! coupling with Bevy), identical API on native and wasm via ewebsock.

use bevy::prelude::Resource;
use zz_core::protocol::{ClientMsg, ServerMsg};
use zz_core::snapshot::{Snapshot, SnapshotDecoder};

/// What a frame's worth of polling yields.
pub enum NetEvent {
    Connected,
    Msg(ServerMsg),
    Snap(Snapshot),
    Closed(String),
}

#[derive(Resource)]
pub struct NetClient {
    sender: Option<ewebsock::WsSender>,
    // Mutex only for Sync (Bevy Resource bound); there is exactly one consumer.
    receiver: Option<std::sync::Mutex<ewebsock::WsReceiver>>,
    decoder: SnapshotDecoder,
    connected: bool,
}

impl NetClient {
    pub fn disconnected() -> Self {
        NetClient {
            sender: None,
            receiver: None,
            decoder: SnapshotDecoder::new(),
            connected: false,
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
            s.send(ewebsock::WsMessage::Binary(frame));
        }
    }

    /// Drain everything that arrived since last frame. Call once per frame.
    pub fn drain(&mut self) -> Vec<NetEvent> {
        let mut out = Vec::new();
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
                        Ok(m) => out.push(NetEvent::Msg(m)),
                        Err(_) => {} // unknown control message: drop
                    }
                }
                ewebsock::WsEvent::Message(ewebsock::WsMessage::Binary(b)) => {
                    if let Ok(snap) = self.decoder.decode(&b) {
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
