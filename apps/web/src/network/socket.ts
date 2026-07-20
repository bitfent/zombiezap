// Thin WebSocket client. Reconnect/backoff is a Phase 3 concern; for the MVP a
// drop simply forfeits (the server already treats it that way).

import {
  BIN_SNAPSHOT,
  SnapshotDecoder,
  type ClientMessage,
  type GameSnapshot,
  type ServerMessage,
} from "@shotante/shared";

const URL = (import.meta as any).env?.VITE_SERVER_URL ?? "ws://localhost:8080";

export class GameSocket {
  private ws: WebSocket | null = null;
  // Per-connection delta decoder; the first frame of every match is a keyframe,
  // so a fresh one each connect always resyncs cleanly.
  private decoder = new SnapshotDecoder();
  /** Voice PCM frames (the leading BIN_VOICE tag byte is already stripped). */
  onBinary: ((data: ArrayBuffer) => void) | null = null;
  /** Decoded per-tick game snapshots (binary BIN_SNAPSHOT frames). */
  onSnapshot: ((snap: GameSnapshot) => void) | null = null;

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
