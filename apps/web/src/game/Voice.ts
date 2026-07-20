// Proximity voice chat: hands-free open mic over the existing game WebSocket.
//
// Capture:  getUserMedia -> AudioWorklet (or ScriptProcessor fallback) -> we
//           resample to 16 kHz mono Int16 PCM and emit ~120ms frames, but ONLY
//           while in a match, not self-muted, and the opponent is within range
//           (so a far-away or idle mic never wastes bandwidth).
// Playback: incoming PCM frames -> AudioBufferSource -> gain (distance fade) ->
//           lowpass (distance muffle) -> stereo panner (direction) -> speakers,
//           with a small jitter buffer so frames don't crackle.
//
// No new dependencies, no WebRTC/STUN/TURN. Works on iOS/Android/desktop as long
// as the page is a secure context (HTTPS or localhost) and the mic was granted
// from a user gesture (the homepage banner).

import { CHAT_PROXIMITY_RADIUS, VOICE_FRAME_MS, VOICE_SAMPLE_RATE } from "@shotante/shared";

/** Live geometry between you and the opponent, recomputed each frame. */
export interface VoiceSpatial {
  dist: number; // horizontal metres to the opponent
  pan: number; // -1 (left) .. +1 (right) relative to where you face
}
export type SpatialProvider = () => VoiceSpatial | null;

export type MicPermission = "unknown" | "granted" | "denied";

const FRAME_SAMPLES = Math.round((VOICE_SAMPLE_RATE * VOICE_FRAME_MS) / 1000); // 1920
const JITTER_S = 0.18; // playback lead so jittery frames don't gap
const MAX_LEAD_S = 0.5; // cap buffered lead so bursty delivery can't grow latency forever
const SMOOTH_TC = 0.08; // time-constant for spatial param ramps
const CUTOFF_NEAR = 8000; // lowpass when right next to them (crisp)
const CUTOFF_FAR = 700; // lowpass at the edge of range (muffled)

export class Voice {
  permission: MicPermission = "unknown";
  /** Wired by the app to the socket's binary send. */
  onFrame: ((buf: ArrayBuffer) => void) | null = null;

  private ctx: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private micTrack: MediaStreamTrack | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private capNode: AudioWorkletNode | ScriptProcessorNode | null = null;
  private silentSink: GainNode | null = null;

  // shared playback chain (sources are created per frame and fan into this)
  private gain: GainNode | null = null;
  private filter: BiquadFilterNode | null = null;
  private panner: StereoPannerNode | null = null;
  private playhead = 0;

  // capture resampler state (ctx rate -> 16 kHz)
  private inRate = 48000;
  private cursor = 0;
  private prevSample = 0;
  private outAccum = new Float32Array(FRAME_SAMPLES * 4);
  private outLen = 0;

  private active = false; // in a match (capture allowed)
  private selfMuted = false;
  private spatial: SpatialProvider = () => null;

  private lastRecvAt = 0; // for "opponent is speaking" UI
  private outLevel = 0; // smoothed outgoing RMS for "you are talking" UI

  setSpatial(fn: SpatialProvider): void {
    this.spatial = fn;
  }

  /** Create + resume the PLAYBACK context from a user gesture, independent of
   *  the mic. A player who denies or never grants the mic still needs this so
   *  they can HEAR the opponent (voice never blocks listening). Safe to call on
   *  every gesture — it's a no-op once the context exists. */
  unlockOutput(): void {
    void this.ensureContext().catch(() => {
      /* no audio output — fine */
    });
  }

  /** True briefly after the opponent's voice last arrived. */
  isReceiving(): boolean {
    return performance.now() - this.lastRecvAt < 250;
  }
  /** True while you are actually transmitting audible speech. */
  isTransmitting(): boolean {
    return this.active && !this.selfMuted && this.outLevel > 0.012;
  }
  get muted(): boolean {
    return this.selfMuted;
  }

  /** Request the mic from a user gesture. Resolves true on grant. */
  async requestMic(): Promise<boolean> {
    if (this.permission === "granted") return true;
    try {
      if (!navigator.mediaDevices?.getUserMedia) {
        this.permission = "denied";
        return false;
      }
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
        video: false,
      });
      this.stream = stream;
      this.micTrack = stream.getAudioTracks()[0] ?? null;
      if (this.micTrack) this.micTrack.enabled = false; // silent until a match starts
      await this.ensureContext();
      await this.buildCapture();
      this.permission = "granted";
      return true;
    } catch {
      this.permission = "denied";
      return false;
    }
  }

  /** Enter a match: allow transmitting and reset the playback buffer. */
  startMatch(): void {
    this.active = true;
    this.playhead = 0;
    this.outLen = 0;
    this.cursor = 0;
    // Ensure the playback context exists/resumes even if the gesture-unlock was
    // missed, so you can always hear the opponent regardless of mic state.
    this.unlockOutput();
    if (this.micTrack) this.micTrack.enabled = !this.selfMuted;
  }

  /** Leave a match: stop transmitting. */
  endMatch(): void {
    this.active = false;
    if (this.micTrack) this.micTrack.enabled = false;
    this.outLen = 0;
    this.outLevel = 0;
  }

  /** Toggle your own mic (privacy). Returns the new muted state. */
  toggleSelfMute(): boolean {
    this.selfMuted = !this.selfMuted;
    if (this.micTrack) this.micTrack.enabled = this.active && !this.selfMuted;
    return this.selfMuted;
  }

  /** Play one incoming PCM frame from the opponent (called by the socket). */
  playFrame(buf: ArrayBuffer): void {
    if (!this.ctx || !this.gain) return;
    this.lastRecvAt = performance.now();
    const pcm = new Int16Array(buf);
    const f = new Float32Array(pcm.length);
    for (let i = 0; i < pcm.length; i++) {
      const v = pcm[i];
      f[i] = v < 0 ? v / 0x8000 : v / 0x7fff;
    }
    const ab = this.ctx.createBuffer(1, f.length, VOICE_SAMPLE_RATE);
    ab.copyToChannel(f, 0);
    const src = this.ctx.createBufferSource();
    src.buffer = ab; // the engine resamples 16k -> ctx rate on playback
    src.connect(this.gain);
    this.applySpatial(this.spatial());
    const now = this.ctx.currentTime;
    // Re-prime after a gap (underrun), and also if a burst pushed the buffer too
    // far ahead — both reset to a fixed jitter lead so latency self-corrects.
    if (this.playhead < now + 0.02 || this.playhead > now + MAX_LEAD_S) {
      this.playhead = now + JITTER_S;
    }
    src.start(this.playhead);
    this.playhead += ab.duration;
  }

  // ── internals ──────────────────────────────────────────────────────────────

  private async ensureContext(): Promise<void> {
    if (!this.ctx) {
      this.ctx = new AudioContext();
      this.inRate = this.ctx.sampleRate;
      this.gain = this.ctx.createGain();
      this.gain.gain.value = 1;
      this.filter = this.ctx.createBiquadFilter();
      this.filter.type = "lowpass";
      this.filter.frequency.value = CUTOFF_NEAR;
      this.gain.connect(this.filter);
      const panner = typeof this.ctx.createStereoPanner === "function" ? this.ctx.createStereoPanner() : null;
      this.panner = panner;
      if (panner) {
        this.filter.connect(panner);
        panner.connect(this.ctx.destination);
      } else {
        this.filter.connect(this.ctx.destination);
      }
      // A muted sink keeps the capture graph "pulling" without local monitoring.
      this.silentSink = this.ctx.createGain();
      this.silentSink.gain.value = 0;
      this.silentSink.connect(this.ctx.destination);
    }
    if (this.ctx.state === "suspended") await this.ctx.resume();
  }

  private async buildCapture(): Promise<void> {
    if (!this.ctx || !this.stream || this.capNode) return;
    this.source = this.ctx.createMediaStreamSource(this.stream);
    // Preferred: AudioWorklet (off the main thread, modern, iOS 14.5+).
    if (this.ctx.audioWorklet) {
      try {
        await this.ctx.audioWorklet.addModule("/voice-capture-worklet.js");
        const node = new AudioWorkletNode(this.ctx, "voice-capture");
        node.port.onmessage = (e) => this.onCaptureChunk(e.data as Float32Array);
        this.source.connect(node);
        node.connect(this.silentSink!);
        this.capNode = node;
        return;
      } catch {
        /* fall through to ScriptProcessor */
      }
    }
    // Fallback: ScriptProcessorNode (deprecated but universally supported).
    const sp = this.ctx.createScriptProcessor(4096, 1, 1);
    sp.onaudioprocess = (e) => this.onCaptureChunk(e.inputBuffer.getChannelData(0));
    this.source.connect(sp);
    sp.connect(this.silentSink!);
    this.capNode = sp;
  }

  private onCaptureChunk(input: Float32Array): void {
    // Only stream when in a match, unmuted, and the opponent is in range.
    if (!this.active || this.selfMuted) {
      this.outLevel *= 0.7;
      return;
    }
    const sp = this.spatial();
    if (!sp || sp.dist > CHAT_PROXIMITY_RADIUS) {
      this.outLevel *= 0.7;
      return;
    }

    const res = this.resample(input);
    let sum = 0;
    for (let i = 0; i < res.length; i++) sum += res[i] * res[i];
    const rms = res.length ? Math.sqrt(sum / res.length) : 0;
    this.outLevel = this.outLevel * 0.7 + rms * 0.3;

    this.append(res);
    while (this.outLen >= FRAME_SAMPLES) {
      const pcm = new Int16Array(FRAME_SAMPLES);
      for (let i = 0; i < FRAME_SAMPLES; i++) {
        const s = Math.max(-1, Math.min(1, this.outAccum[i]));
        pcm[i] = s < 0 ? s * 0x8000 : s * 0x7fff;
      }
      this.onFrame?.(pcm.buffer);
      this.outAccum.copyWithin(0, FRAME_SAMPLES, this.outLen);
      this.outLen -= FRAME_SAMPLES;
    }
  }

  private append(s: Float32Array): void {
    const need = this.outLen + s.length;
    if (need > this.outAccum.length) {
      const grow = new Float32Array(Math.max(need, this.outAccum.length * 2));
      grow.set(this.outAccum.subarray(0, this.outLen));
      this.outAccum = grow;
    }
    this.outAccum.set(s, this.outLen);
    this.outLen += s.length;
  }

  /** Streaming linear resample from the context rate to 16 kHz. */
  private resample(input: Float32Array): Float32Array {
    const len = input.length;
    if (len === 0) return input;
    if (this.inRate === VOICE_SAMPLE_RATE) {
      this.prevSample = input[len - 1];
      return input;
    }
    const ratio = this.inRate / VOICE_SAMPLE_RATE;
    const out: number[] = [];
    let pos = this.cursor;
    while (pos < len) {
      const i = Math.floor(pos);
      const frac = pos - i;
      const a = i < 0 ? this.prevSample : input[i];
      const j = i + 1;
      const b = j < 0 ? this.prevSample : j < len ? input[j] : input[len - 1];
      out.push(a + (b - a) * frac);
      pos += ratio;
    }
    this.cursor = pos - len; // carry the fractional position into the next chunk
    this.prevSample = input[len - 1];
    return Float32Array.from(out);
  }

  private applySpatial(sp: VoiceSpatial | null): void {
    if (!this.ctx || !this.gain || !this.filter) return;
    const t = this.ctx.currentTime;
    let vol = 1;
    let cutoff = CUTOFF_NEAR;
    let pan = 0;
    if (sp) {
      const k = Math.max(0, Math.min(1, sp.dist / CHAT_PROXIMITY_RADIUS)); // 0 close .. 1 far
      vol = Math.max(0, 1 - k); // fade to silence exactly at the radius -> seamless cut
      cutoff = CUTOFF_FAR + (CUTOFF_NEAR - CUTOFF_FAR) * (1 - k); // muffle with distance
      pan = Math.max(-1, Math.min(1, sp.pan));
    }
    this.gain.gain.setTargetAtTime(vol, t, SMOOTH_TC);
    this.filter.frequency.setTargetAtTime(cutoff, t, SMOOTH_TC);
    this.panner?.pan.setTargetAtTime(pan, t, SMOOTH_TC);
  }
}
