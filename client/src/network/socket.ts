// Thin WebSocket client. Reconnect/backoff is a Phase 3 concern; for the MVP a
// drop simply forfeits (the server already treats it that way).

import {
  BIN_SNAPSHOT,
  SnapshotDecoder,
  type ClientMessage,
  type ZzSnapshot,
  type ServerMessage,
} from "@shotante/shared";

// zz-server upgrades WebSockets on /ws (the root serves the web build).
// Same-origin by default so the built client works wherever it's served
// (localhost, LAN phone, deployed); the Vite dev server (port 5174) isn't
// the game server, so dev falls back to localhost:8080.
const URL =
  (import.meta as any).env?.VITE_SERVER_URL ??
  (location.port === "5174"
    ? "ws://localhost:8080/ws"
    : `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`);

export class GameSocket {
  private ws: WebSocket | null = null;
  // Per-connection delta decoder; the first frame of every match is a keyframe,
  // so a fresh one each connect always resyncs cleanly.
  private decoder = new SnapshotDecoder();
  /** Voice PCM frames (the leading BIN_VOICE tag byte is already stripped). */
  onBinary: ((data: ArrayBuffer) => void) | null = null;
  /** Decoded per-tick game snapshots (binary BIN_SNAPSHOT frames). */
  onSnapshot: ((snap: ZzSnapshot) => void) | null = null;

  connect(onMessage: (msg: ServerMessage) => void, onClose: () => void): Promise<void> {
    return new Promise((resolve, reject) => {
      this.decoder = new SnapshotDecoder();
      const ws = new WebSocket(URL);
      ws.binaryType = "arraybuffer";
      ws.onopen = () => {
        this.ws = ws;
        resolve();
      };
      ws.onmessage = (ev) => {
        // Binary frames carry a leading tag byte: snapshot vs voice. Text = JSON
        // control messages.
        if (ev.data instanceof ArrayBuffer) {
          const tag = new Uint8Array(ev.data, 0, 1)[0];
          if (tag === BIN_SNAPSHOT) this.onSnapshot?.(this.decoder.decode(ev.data));
          else this.onBinary?.(ev.data.slice(1)); // voice: hand on the PCM, tag stripped
          return;
        }
        try {
          onMessage(JSON.parse(String(ev.data)) as ServerMessage);
        } catch {
          /* drop malformed frames */
        }
      };
      ws.onerror = () => reject(new Error("could not reach game server"));
      ws.onclose = () => {
        this.ws = null;
        onClose();
      };
    });
  }

  send(msg: ClientMessage): void {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  /** Send a raw binary audio frame (voice). Dropped if the socket isn't open. */
  sendBinary(data: ArrayBuffer | ArrayBufferView): void {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(data);
  }

  close(): void {
    this.ws?.close();
    this.ws = null;
  }

  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }
}
