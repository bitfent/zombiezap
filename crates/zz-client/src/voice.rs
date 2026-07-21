//! Proximity voice: capture → BIN_VOICE send, receive → jitter → fade/pan playback.
//!
//! Pure jitter / fade / pan logic is unit-tested. Capture sits behind
//! [`CaptureSource`] so a fake source can drive an end-to-end unit test without
//! a real microphone. Nothing blocks a frame; overflow drops oldest.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;
use std::time::Duration;

use bevy::audio::{AddAudioSource, Decodable, Source, Volume};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use zz_core::constants::CHAT_PROXIMITY_RADIUS;
use zz_core::protocol::encode_voice;
#[cfg(any(not(target_arch = "wasm32"), test))]
use zz_core::protocol::MAX_VOICE_PAYLOAD;

use crate::game::{Predicted, RemotePlayer, Session};
use crate::net::NetClient;
#[cfg(target_arch = "wasm32")]
use crate::platform;

// ── wire / audio constants ─────────────────────────────────────────────────

/// Capture / playback sample rate (matches server BIN_VOICE contract).
pub const VOICE_SAMPLE_RATE: u32 = 16_000;
/// Nominal frame duration in milliseconds (~120 ms).
pub const VOICE_FRAME_MS: u32 = 120;
/// Samples per frame at 16 kHz × 120 ms.
#[cfg(any(not(target_arch = "wasm32"), test))]
pub const FRAME_SAMPLES: usize = (VOICE_SAMPLE_RATE as usize * VOICE_FRAME_MS as usize) / 1000;
/// Bytes per mono i16 frame (exactly [`MAX_VOICE_PAYLOAD`]).
#[cfg(any(not(target_arch = "wasm32"), test))]
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2;

/// Distance (m) at which fade begins; full volume inside this radius.
pub const FADE_START_M: f32 = 10.0;

/// Jitter: start playout after this many frames buffered.
const JITTER_START: usize = 2;
/// Jitter: max frames retained per speaker (overflow drops oldest).
const JITTER_CAP: usize = 4;
/// Wall-clock seconds per frame for playout clock.
const FRAME_SECS: f32 = VOICE_FRAME_MS as f32 / 1000.0;
/// How long a remote counts as "speaking" for HUD (seconds).
const SPEAKING_HOLD_S: f32 = 0.35;

// ── pure spatial math (unit-tested) ────────────────────────────────────────

/// Distance → linear gain in `[0, 1]`.
///
/// Full volume out to [`FADE_START_M`], then linear fade to silence at
/// [`CHAT_PROXIMITY_RADIUS`] (server relay radius, 25 m).
pub fn distance_gain(dist: f32) -> f32 {
    if !(dist.is_finite()) || dist < 0.0 {
        return 0.0;
    }
    if dist >= CHAT_PROXIMITY_RADIUS {
        return 0.0;
    }
    if dist <= FADE_START_M {
        return 1.0;
    }
    let span = CHAT_PROXIMITY_RADIUS - FADE_START_M;
    if span <= f32::EPSILON {
        return 0.0;
    }
    ((CHAT_PROXIMITY_RADIUS - dist) / span).clamp(0.0, 1.0)
}

/// Horizontal bearing of a speaker relative to the listener's yaw → stereo pan
/// in `[-1, 1]` (left…right).
///
/// `listener_yaw` is the camera yaw (rad, Bevy/Y-up: 0 faces −Z). `dx`/`dz`
/// are world-space offsets from listener to speaker on the XZ plane.
pub fn bearing_pan(listener_yaw: f32, dx: f32, dz: f32) -> f32 {
    if !dx.is_finite() || !dz.is_finite() || !listener_yaw.is_finite() {
        return 0.0;
    }
    let horiz = (dx * dx + dz * dz).sqrt();
    if horiz < 1e-4 {
        return 0.0;
    }
    // World angle of the offset (atan2(x, -z) matches yaw convention used by
    // the FPS controller: yaw 0 looks along −Z, positive yaw turns left).
    let world_bearing = dx.atan2(-dz);
    // Relative angle: positive = speaker is to the listener's right.
    let mut rel = world_bearing - listener_yaw;
    // Wrap to (−π, π].
    let pi = core::f32::consts::PI;
    while rel > pi {
        rel -= 2.0 * pi;
    }
    while rel <= -pi {
        rel += 2.0 * pi;
    }
    // Map ±90° (and beyond) into [-1, 1] via sin: straight ahead → 0,
    // full right → +1, full left → −1.
    rel.sin().clamp(-1.0, 1.0)
}

/// Apply gain and stereo pan to mono i16 PCM → interleaved stereo f32.
///
/// Equal-power pan: `L = g·cos((p+1)·π/4)`, `R = g·sin((p+1)·π/4)` with
/// `p ∈ [-1, 1]`. Center (`p = 0`) puts ~0.707·g on each ear.
pub fn apply_gain_pan(mono_i16: &[i16], gain: f32, pan: f32) -> Vec<f32> {
    let g = gain.clamp(0.0, 1.0);
    let p = pan.clamp(-1.0, 1.0);
    let angle = (p + 1.0) * 0.25 * core::f32::consts::PI; // 0..π/2
    let l_eq = g * angle.cos();
    let r_eq = g * angle.sin();
    let mut out = Vec::with_capacity(mono_i16.len() * 2);
    for &s in mono_i16 {
        let f = i16_to_f32(s);
        out.push(f * l_eq);
        out.push(f * r_eq);
    }
    out
}

fn i16_to_f32(s: i16) -> f32 {
    if s < 0 {
        s as f32 / 32768.0
    } else {
        s as f32 / 32767.0
    }
}

/// Decode little-endian mono i16 PCM bytes into samples. Odd trailing byte dropped.
pub fn pcm_bytes_to_i16(pcm: &[u8]) -> Vec<i16> {
    let n = pcm.len() / 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let lo = pcm[i * 2];
        let hi = pcm[i * 2 + 1];
        out.push(i16::from_le_bytes([lo, hi]));
    }
    out
}

/// Encode mono i16 samples as little-endian PCM bytes (for capture → wire).
#[cfg(test)]
pub fn i16_to_pcm_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

// ── jitter buffer (unit-tested) ────────────────────────────────────────────

/// Per-speaker reorder buffer. Frames carry a sequence number so unit tests
/// can inject out-of-order arrivals; the live path assigns monotonic seqs on
/// receive (WebSocket is ordered, but the buffer still bounds latency).
#[derive(Debug, Default)]
pub struct JitterBuffer {
    pending: BTreeMap<u64, Vec<i16>>,
    next_seq: u64,
    primed: bool,
    started: bool,
    capacity: usize,
    start_threshold: usize,
}

impl JitterBuffer {
    pub fn new(capacity: usize, start_threshold: usize) -> Self {
        Self {
            pending: BTreeMap::new(),
            next_seq: 0,
            primed: false,
            started: false,
            capacity: capacity.max(1),
            start_threshold: start_threshold.max(1),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(JITTER_CAP, JITTER_START)
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Insert a frame. Duplicate seq ignored. Overflow drops the **oldest**
    /// (lowest seq). Frames older than `next_seq` (already played / skipped)
    /// are dropped.
    pub fn push(&mut self, seq: u64, samples: Vec<i16>) {
        if samples.is_empty() {
            return;
        }
        if self.primed && seq < self.next_seq {
            return;
        }
        if self.pending.contains_key(&seq) {
            return;
        }
        self.pending.insert(seq, samples);
        while self.pending.len() > self.capacity {
            // Drop oldest.
            if let Some(k) = self.pending.keys().next().copied() {
                self.pending.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Pop the next ordered frame for playback.
    ///
    /// * Before start: returns `None` until `start_threshold` frames are buffered,
    ///   then primes `next_seq` to the lowest buffered seq.
    /// * After start: returns the frame for `next_seq` if present; on gap or
    ///   empty buffer returns `None` (**silence / starvation**) without
    ///   advancing (late packets can still fill the hole). Call
    ///   [`skip_gap`](Self::skip_gap) if you want to jump past a persistent hole.
    pub fn pop(&mut self) -> Option<Vec<i16>> {
        if !self.started {
            if self.pending.len() < self.start_threshold {
                return None;
            }
            // Prime to the earliest buffered sequence.
            if let Some(&min) = self.pending.keys().next() {
                self.next_seq = min;
                self.primed = true;
                self.started = true;
            } else {
                return None;
            }
        }
        if let Some(samples) = self.pending.remove(&self.next_seq) {
            self.next_seq = self.next_seq.saturating_add(1);
            Some(samples)
        } else {
            // Starvation / gap → silence.
            None
        }
    }

}

// ── capture trait + fake (testability) ─────────────────────────────────────

/// Backend that yields complete 16 kHz mono i16 LE PCM frames (~120 ms).
///
/// Implemented by native cpal, wasm JS bridge, and [`FakeCapture`] for tests.
/// Not `Send`: native `cpal::Stream` is main-thread-only on some hosts.
pub trait CaptureSource {
    /// Enable/disable mic consumption (privacy mute). Does not release the device.
    fn set_enabled(&mut self, enabled: bool);
    /// Non-blocking: drain any completed frames since last poll.
    fn drain_frames(&mut self) -> Vec<Vec<u8>>;
    /// Request mic permission / open the device (idempotent). Async on wasm.
    fn ensure_started(&mut self);
    /// True once capture is producing (or ready to produce) frames.
    fn is_ready(&self) -> bool;
}

/// Test double: push frames in, drain them out.
#[cfg(test)]
#[derive(Default)]
pub struct FakeCapture {
    enabled: bool,
    ready: bool,
    queue: VecDeque<Vec<u8>>,
}

#[cfg(test)]
impl FakeCapture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_frame(&mut self, pcm: Vec<u8>) {
        self.queue.push_back(pcm);
    }
}

#[cfg(test)]
impl CaptureSource for FakeCapture {
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        if !self.enabled {
            self.queue.clear();
            return Vec::new();
        }
        self.queue.drain(..).collect()
    }

    fn ensure_started(&mut self) {
        self.ready = true;
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}

/// Build ~120 ms mono i16 LE frames from float samples at an arbitrary input rate.
/// Used by native cpal capture and unit tests; wasm frames in JS.
#[cfg(any(not(target_arch = "wasm32"), test))]
#[derive(Debug)]
pub struct FrameAssembler {
    in_rate: u32,
    cursor: f64,
    prev: f32,
    accum: Vec<f32>,
}

#[cfg(any(not(target_arch = "wasm32"), test))]
impl FrameAssembler {
    pub fn new(in_rate: u32) -> Self {
        Self {
            in_rate: in_rate.max(1),
            cursor: 0.0,
            prev: 0.0,
            accum: Vec::with_capacity(FRAME_SAMPLES * 2),
        }
    }

    /// Push a mono float chunk; returns zero or more complete wire frames.
    pub fn push_f32(&mut self, input: &[f32]) -> Vec<Vec<u8>> {
        if input.is_empty() {
            return Vec::new();
        }
        let resampled = self.resample(input);
        self.accum.extend_from_slice(&resampled);
        let mut frames = Vec::new();
        while self.accum.len() >= FRAME_SAMPLES {
            let chunk: Vec<f32> = self.accum.drain(..FRAME_SAMPLES).collect();
            let mut pcm = Vec::with_capacity(FRAME_BYTES);
            for s in chunk {
                let c = s.clamp(-1.0, 1.0);
                let i = if c < 0.0 {
                    (c * 32768.0) as i16
                } else {
                    (c * 32767.0) as i16
                };
                pcm.extend_from_slice(&i.to_le_bytes());
            }
            // Cap: never exceed wire max (should be exact FRAME_BYTES).
            if pcm.len() > MAX_VOICE_PAYLOAD {
                pcm.truncate(MAX_VOICE_PAYLOAD);
                if pcm.len() % 2 == 1 {
                    pcm.pop();
                }
            }
            if !pcm.is_empty() {
                frames.push(pcm);
            }
        }
        frames
    }

    fn resample(&mut self, input: &[f32]) -> Vec<f32> {
        let len = input.len();
        if self.in_rate == VOICE_SAMPLE_RATE {
            self.prev = input[len - 1];
            return input.to_vec();
        }
        let ratio = self.in_rate as f64 / VOICE_SAMPLE_RATE as f64;
        let mut out = Vec::with_capacity(((len as f64) / ratio).ceil() as usize + 1);
        let mut pos = self.cursor;
        while pos < len as f64 {
            let i = pos.floor() as isize;
            let frac = (pos - pos.floor()) as f32;
            let a = if i < 0 {
                self.prev
            } else if (i as usize) < len {
                input[i as usize]
            } else {
                input[len - 1]
            };
            let j = i + 1;
            let b = if j < 0 {
                self.prev
            } else if (j as usize) < len {
                input[j as usize]
            } else {
                input[len - 1]
            };
            out.push(a + (b - a) * frac);
            pos += ratio;
        }
        self.cursor = pos - len as f64;
        self.prev = input[len - 1];
        out
    }
}

// ── platform capture backends ──────────────────────────────────────────────

/// Shared capture used by the live client (native cpal or wasm bridge).
struct LiveCapture {
    enabled: bool,
    ready: bool,
    #[cfg(not(target_arch = "wasm32"))]
    native: Option<NativeCpalState>,
}

impl LiveCapture {
    fn new() -> Self {
        Self {
            enabled: false,
            ready: false,
            #[cfg(not(target_arch = "wasm32"))]
            native: None,
        }
    }
}

impl CaptureSource for LiveCapture {
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(n) = self.native.as_mut() {
            n.set_enabled(enabled);
        }
        #[cfg(target_arch = "wasm32")]
        {
            platform::voice_set_tx_enabled(enabled);
        }
    }

    fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        if !self.enabled {
            // Still drain/drop platform queues so they don't grow while muted.
            #[cfg(target_arch = "wasm32")]
            {
                let _ = platform::voice_drain_pcm_frames();
            }
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(n) = self.native.as_mut() {
                let _ = n.drain_frames();
            }
            return Vec::new();
        }
        #[cfg(target_arch = "wasm32")]
        {
            platform::voice_drain_pcm_frames()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(n) = self.native.as_mut() {
                n.drain_frames()
            } else {
                Vec::new()
            }
        }
    }

    fn ensure_started(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            platform::voice_request_mic();
            self.ready = platform::voice_mic_ready();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.native.is_none() {
                match NativeCpalState::start() {
                    Ok(n) => {
                        self.native = Some(n);
                        self.ready = true;
                    }
                    Err(e) => {
                        warn!("voice: cpal input failed: {e}");
                        self.ready = false;
                    }
                }
            } else {
                self.ready = true;
            }
            if let Some(n) = self.native.as_mut() {
                n.set_enabled(self.enabled);
            }
        }
    }

    fn is_ready(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.ready || platform::voice_mic_ready()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.ready
        }
    }
}

// ── native cpal input ──────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
struct NativeCpalState {
    // Keep stream alive.
    _stream: cpal::Stream,
    /// Float mono chunks from the callback.
    raw: Arc<Mutex<VecDeque<Vec<f32>>>>,
    assembler: FrameAssembler,
    enabled: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeCpalState {
    fn start() -> Result<Self, String> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("input config: {e}"))?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let raw: Arc<Mutex<VecDeque<Vec<f32>>>> = Arc::new(Mutex::new(VecDeque::new()));
        let raw_cb = Arc::clone(&raw);
        let enabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let enabled_cb = Arc::clone(&enabled);

        let err_fn = |e| eprintln!("voice cpal stream error: {e}");

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let conf: cpal::StreamConfig = config.clone().into();
                device
                    .build_input_stream(
                        &conf,
                        move |data: &[f32], _| {
                            if !enabled_cb.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            let mono = downmix_f32(data, channels);
                            if let Ok(mut q) = raw_cb.lock() {
                                // Cap queued chunks (~0.5 s) — drop oldest.
                                while q.len() > 32 {
                                    q.pop_front();
                                }
                                q.push_back(mono);
                            }
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("build f32 stream: {e}"))?
            }
            cpal::SampleFormat::I16 => {
                let conf: cpal::StreamConfig = config.clone().into();
                device
                    .build_input_stream(
                        &conf,
                        move |data: &[i16], _| {
                            if !enabled_cb.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            let mut f = Vec::with_capacity(data.len());
                            for &s in data {
                                f.push(i16_to_f32(s));
                            }
                            let mono = downmix_f32(&f, channels);
                            if let Ok(mut q) = raw_cb.lock() {
                                while q.len() > 32 {
                                    q.pop_front();
                                }
                                q.push_back(mono);
                            }
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("build i16 stream: {e}"))?
            }
            other => {
                return Err(format!("unsupported input sample format: {other:?}"));
            }
        };
        stream.play().map_err(|e| format!("play stream: {e}"))?;

        Ok(Self {
            _stream: stream,
            raw,
            assembler: FrameAssembler::new(sample_rate),
            enabled,
        })
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled
            .store(on, std::sync::atomic::Ordering::Relaxed);
        if !on {
            if let Ok(mut q) = self.raw.lock() {
                q.clear();
            }
            self.assembler = FrameAssembler::new(self.assembler.in_rate);
        }
    }

    fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        let chunks: Vec<Vec<f32>> = if let Ok(mut q) = self.raw.lock() {
            q.drain(..).collect()
        } else {
            Vec::new()
        };
        let mut frames = Vec::new();
        for c in chunks {
            frames.extend(self.assembler.push_f32(&c));
        }
        // Cap outbound frames per poll so we never flood the socket.
        if frames.len() > 4 {
            frames.drain(0..frames.len() - 4);
        }
        frames
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn downmix_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    let n = data.len() / channels;
    let mut mono = Vec::with_capacity(n);
    for i in 0..n {
        let mut s = 0.0f32;
        for c in 0..channels {
            s += data[i * channels + c];
        }
        mono.push(s / channels as f32);
    }
    mono
}

// ── playback Decodable (stereo f32 chunks) ─────────────────────────────────

#[derive(Asset, TypePath, Clone)]
struct VoiceClip {
    /// Interleaved stereo f32 samples.
    samples: Arc<[f32]>,
}

struct VoiceDecoder {
    samples: Arc<[f32]>,
    index: usize,
    sample_rate: NonZeroU32,
    channels: NonZeroU16,
}

impl VoiceDecoder {
    fn new(samples: Arc<[f32]>) -> Self {
        Self {
            samples,
            index: 0,
            sample_rate: NonZeroU32::new(VOICE_SAMPLE_RATE).expect("rate > 0"),
            channels: NonZeroU16::new(2).expect("stereo"),
        }
    }
}

impl Iterator for VoiceDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.samples.len() {
            return None;
        }
        let s = self.samples[self.index];
        self.index += 1;
        Some(s)
    }
}

impl Source for VoiceDecoder {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.samples.len().saturating_sub(self.index))
    }

    fn channels(&self) -> NonZeroU16 {
        self.channels
    }

    fn sample_rate(&self) -> NonZeroU32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        let frames = self.samples.len() / 2;
        let secs = frames as f64 / VOICE_SAMPLE_RATE as f64;
        Some(Duration::from_secs_f64(secs))
    }
}

impl Decodable for VoiceClip {
    type Decoder = VoiceDecoder;

    fn decoder(&self) -> Self::Decoder {
        VoiceDecoder::new(Arc::clone(&self.samples))
    }
}

// ── resources + plugin ─────────────────────────────────────────────────────

/// Incoming voice frames from `net_poll` (slot, raw PCM bytes).
#[derive(Resource, Default)]
pub struct VoiceRx(pub VecDeque<(u8, Vec<u8>)>);

/// Mute / speaking state for HUD and capture control.
#[derive(Resource)]
pub struct VoiceState {
    /// Privacy mute — default **true** (open-mic only after user unmutes).
    pub muted: bool,
    /// Mic permission / device open succeeded at least once.
    pub mic_ready: bool,
    /// Mic permission denied (or no device).
    pub mic_denied: bool,
    /// Remote slots that spoke recently → hold timer (seconds remaining).
    pub speaking: HashMap<u8, f32>,
    /// True while local capture is streaming audible frames this session.
    pub self_live: bool,
}

impl Default for VoiceState {
    fn default() -> Self {
        Self {
            muted: true,
            mic_ready: false,
            mic_denied: false,
            speaking: HashMap::new(),
            self_live: false,
        }
    }
}

struct SpeakerPlay {
    jitter: JitterBuffer,
    /// Playout clock accumulator (seconds).
    accum: f32,
    /// Next seq assigned on receive (live path).
    recv_seq: u64,
    /// Quiet timer — clear jitter after silence.
    quiet: f32,
}

impl Default for SpeakerPlay {
    fn default() -> Self {
        Self {
            jitter: JitterBuffer::with_defaults(),
            accum: 0.0,
            recv_seq: 0,
            quiet: 0.0,
        }
    }
}

#[derive(Resource, Default)]
struct Speakers(HashMap<u8, SpeakerPlay>);

/// Owns the live capture backend. **NonSend** because native `cpal::Stream`
/// is `!Send`/`!Sync` on some hosts (CoreAudio). Main thread only is fine —
/// capture callbacks push into `Arc<Mutex<_>>` queues.
struct CaptureBox {
    inner: LiveCapture,
}

pub struct VoicePlugin;

impl Plugin for VoicePlugin {
    fn build(&self, app: &mut App) {
        app.add_audio_source::<VoiceClip>()
            .init_resource::<VoiceState>()
            .init_resource::<VoiceRx>()
            .init_resource::<Speakers>()
            .insert_non_send(CaptureBox {
                inner: LiveCapture::new(),
            })
            .add_systems(
                Update,
                (
                    voice_mute_toggle.run_if(crate::game::in_match),
                    voice_capture_send.run_if(crate::game::in_match),
                    voice_receive_play.run_if(crate::game::in_match),
                    voice_on_session_change,
                )
                    .chain(),
            );
    }
}

// ── systems ────────────────────────────────────────────────────────────────

fn voice_mute_toggle(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<VoiceState>,
    mut capture: NonSendMut<CaptureBox>,
) {
    if !keys.just_pressed(KeyCode::KeyM) {
        return;
    }
    if state.muted {
        // Unmute: request mic on first unmute (never at startup).
        state.muted = false;
        capture.inner.ensure_started();
        capture.inner.set_enabled(true);
        state.mic_ready = capture.inner.is_ready();
        #[cfg(target_arch = "wasm32")]
        {
            // Permission is async — refresh flags from platform each toggle.
            if platform::voice_mic_denied() {
                state.mic_denied = true;
                state.muted = true;
                capture.inner.set_enabled(false);
            }
        }
    } else {
        state.muted = true;
        state.self_live = false;
        capture.inner.set_enabled(false);
    }
}

fn voice_capture_send(
    mut state: ResMut<VoiceState>,
    mut capture: NonSendMut<CaptureBox>,
    mut net: ResMut<NetClient>,
    session: Res<Session>,
) {
    if !matches!(*session, Session::Playing { .. }) {
        state.self_live = false;
        return;
    }
    if state.muted {
        state.self_live = false;
        // Keep wasm permission progress updated while muted after a request.
        #[cfg(target_arch = "wasm32")]
        {
            state.mic_ready = platform::voice_mic_ready();
            if platform::voice_mic_denied() {
                state.mic_denied = true;
            }
        }
        return;
    }

    // Refresh readiness (wasm grant may complete a few frames after unmute).
    #[cfg(target_arch = "wasm32")]
    {
        capture.inner.ensure_started();
        state.mic_ready = capture.inner.is_ready();
        if platform::voice_mic_denied() {
            state.mic_denied = true;
            state.muted = true;
            capture.inner.set_enabled(false);
            state.self_live = false;
            return;
        }
        if state.mic_ready {
            capture.inner.set_enabled(true);
        }
    }

    let frames = capture.inner.drain_frames();
    state.self_live = !frames.is_empty();
    for pcm in frames {
        // Slot byte is server-rewritten; send 0.
        if let Some(frame) = encode_voice(0, &pcm) {
            net.send_bin(frame);
        }
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system params
fn voice_receive_play(
    time: Res<Time>,
    mut rx: ResMut<VoiceRx>,
    mut speakers: ResMut<Speakers>,
    mut state: ResMut<VoiceState>,
    mut clips: ResMut<Assets<VoiceClip>>,
    mut commands: Commands,
    predicted: Res<Predicted>,
    remotes: Query<(&RemotePlayer, &Transform)>,
    session: Res<Session>,
) {
    let my_slot = match *session {
        Session::Playing { my_slot } | Session::Ended { my_slot } => my_slot,
        _ => return,
    };

    // Ingest wire frames.
    while let Some((slot, pcm)) = rx.0.pop_front() {
        if slot == my_slot {
            continue; // never play self (server also suppresses echo)
        }
        let samples = pcm_bytes_to_i16(&pcm);
        if samples.is_empty() {
            continue;
        }
        let sp = speakers.0.entry(slot).or_default();
        let seq = sp.recv_seq;
        sp.recv_seq = sp.recv_seq.saturating_add(1);
        sp.jitter.push(seq, samples);
        sp.quiet = 0.0;
        state.speaking.insert(slot, SPEAKING_HOLD_S);
    }

    // Decay speaking timers.
    let dt = time.delta_secs();
    state.speaking.retain(|_, t| {
        *t -= dt;
        *t > 0.0
    });

    // Listener pose.
    let listener = Vec3::new(predicted.body.x, predicted.body.y, predicted.body.z);
    let yaw = predicted.yaw;

    // Build slot → world pos from interpolated remotes.
    let mut positions: HashMap<u8, Vec3> = HashMap::new();
    for (rp, tf) in remotes.iter() {
        positions.insert(rp.slot, tf.translation);
    }

    // Playout each speaker on a ~120 ms clock.
    let mut finished_slots = Vec::new();
    for (&slot, sp) in speakers.0.iter_mut() {
        sp.accum += dt;
        let mut played = false;
        while sp.accum >= FRAME_SECS {
            sp.accum -= FRAME_SECS;
            match sp.jitter.pop() {
                Some(mono) => {
                    played = true;
                    let pos = positions.get(&slot).copied().unwrap_or(listener);
                    let dx = pos.x - listener.x;
                    let dz = pos.z - listener.z;
                    let dist = (dx * dx + dz * dz).sqrt(); // horizontal
                    let gain = distance_gain(dist);
                    if gain < 0.01 {
                        continue;
                    }
                    let pan = bearing_pan(yaw, dx, dz);
                    let stereo = apply_gain_pan(&mono, gain, pan);
                    if stereo.is_empty() {
                        continue;
                    }
                    let handle = clips.add(VoiceClip {
                        samples: Arc::from(stereo),
                    });
                    commands.spawn((
                        AudioPlayer(handle),
                        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(1.0)),
                    ));
                }
                None => {
                    // Starvation this slot-tick: leave silence; don't spin the clock.
                    break;
                }
            }
        }
        if played {
            sp.quiet = 0.0;
        } else {
            sp.quiet += dt;
            if sp.quiet > 1.0 && sp.jitter.is_empty() {
                finished_slots.push(slot);
            }
        }
    }
    for s in finished_slots {
        speakers.0.remove(&s);
    }
}

fn voice_on_session_change(
    session: Res<Session>,
    mut state: ResMut<VoiceState>,
    mut speakers: ResMut<Speakers>,
    mut rx: ResMut<VoiceRx>,
    mut capture: NonSendMut<CaptureBox>,
) {
    if !session.is_changed() {
        return;
    }
    let in_match = matches!(*session, Session::Playing { .. } | Session::Ended { .. });
    if !in_match {
        // Leave match: stop tx, clear playout. Stay muted for next match
        // (privacy). Mic device may remain open on native — re-enable on unmute.
        capture.inner.set_enabled(false);
        state.self_live = false;
        state.speaking.clear();
        speakers.0.clear();
        rx.0.clear();
        // Re-assert muted default when leaving play? Spec: default muted.
        // Keep user's preference across lobby re-entry for the session — only
        // hard-reset on full disconnect is nicer UX; privacy default is at boot.
        // We re-mute on leaving match for safety:
        state.muted = true;
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_core::protocol::{decode_voice, encode_voice};

    #[test]
    fn distance_gain_bounds_and_fade() {
        assert!((distance_gain(0.0) - 1.0).abs() < 1e-5);
        assert!((distance_gain(FADE_START_M) - 1.0).abs() < 1e-5);
        assert!((distance_gain(FADE_START_M * 0.5) - 1.0).abs() < 1e-5);
        let mid = (FADE_START_M + CHAT_PROXIMITY_RADIUS) * 0.5;
        let g = distance_gain(mid);
        assert!(g > 0.0 && g < 1.0, "mid gain {g}");
        assert_eq!(distance_gain(CHAT_PROXIMITY_RADIUS), 0.0);
        assert_eq!(distance_gain(CHAT_PROXIMITY_RADIUS + 5.0), 0.0);
        assert_eq!(distance_gain(-1.0), 0.0);
        // Monotone in the fade band.
        let a = distance_gain(12.0);
        let b = distance_gain(20.0);
        assert!(a > b);
        // Bounded.
        for d in [0.0, 5.0, 10.0, 15.0, 24.9, 25.0, 100.0] {
            let g = distance_gain(d);
            assert!((0.0..=1.0).contains(&g), "gain {g} at {d}");
        }
    }

    #[test]
    fn bearing_pan_bounds_and_sides() {
        // Listener at origin facing −Z (yaw 0). Speaker on +X → right → +pan.
        let p_right = bearing_pan(0.0, 10.0, 0.0);
        assert!(p_right > 0.5, "right pan {p_right}");
        let p_left = bearing_pan(0.0, -10.0, 0.0);
        assert!(p_left < -0.5, "left pan {p_left}");
        let p_front = bearing_pan(0.0, 0.0, -10.0);
        assert!(p_front.abs() < 0.1, "front pan {p_front}");
        // Coincident → 0.
        assert_eq!(bearing_pan(0.0, 0.0, 0.0), 0.0);
        for (yaw, dx, dz) in [
            (0.0, 1.0, 0.0),
            (1.0, 3.0, -2.0),
            (-2.0, -4.0, 1.0),
            (0.5, 0.0, 1.0),
        ] {
            let p = bearing_pan(yaw, dx, dz);
            assert!((-1.0..=1.0).contains(&p), "pan {p}");
        }
    }

    #[test]
    fn jitter_ordered_from_ooo() {
        let mut jb = JitterBuffer::new(4, 2);
        // Push out of order: 1, then 0, then 2.
        jb.push(1, vec![1]);
        assert!(jb.pop().is_none(), "need 2 frames to start");
        jb.push(0, vec![0]);
        jb.push(2, vec![2]);
        assert_eq!(jb.pop(), Some(vec![0]));
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert!(jb.pop().is_none(), "starvation → silence");
    }

    #[test]
    fn jitter_overflow_drops_oldest() {
        let mut jb = JitterBuffer::new(2, 2);
        jb.push(0, vec![0]);
        jb.push(1, vec![1]);
        jb.push(2, vec![2]); // drops oldest (0)
        // Start: min key is 1.
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert!(jb.pop().is_none());
    }

    #[test]
    fn jitter_starvation_silence() {
        let mut jb = JitterBuffer::new(4, 2);
        jb.push(0, vec![10]);
        jb.push(1, vec![11]);
        assert_eq!(jb.pop(), Some(vec![10]));
        assert_eq!(jb.pop(), Some(vec![11]));
        // Empty → silence.
        assert!(jb.pop().is_none());
        assert!(jb.pop().is_none());
        // Late fill continues.
        jb.push(2, vec![12]);
        assert_eq!(jb.pop(), Some(vec![12]));
    }

    #[test]
    fn fake_capture_encode_decode_jitter_round_trip() {
        // Build a synthetic tone frame as i16 PCM.
        let mut samples = Vec::with_capacity(FRAME_SAMPLES);
        for i in 0..FRAME_SAMPLES {
            let t = i as f32 / VOICE_SAMPLE_RATE as f32;
            let s = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
            samples.push(if s < 0.0 {
                (s * 32768.0) as i16
            } else {
                (s * 32767.0) as i16
            });
        }
        let pcm = i16_to_pcm_bytes(&samples);
        assert_eq!(pcm.len(), FRAME_BYTES);

        let mut fake = FakeCapture::new();
        fake.ensure_started();
        fake.set_enabled(true);
        fake.push_frame(pcm.clone());

        let drained = fake.drain_frames();
        assert_eq!(drained.len(), 1);

        let frame = encode_voice(0, &drained[0]).expect("encode");
        let (slot, got_pcm) = decode_voice(&frame).expect("decode");
        assert_eq!(slot, 0);
        assert_eq!(got_pcm, pcm.as_slice());

        let mono = pcm_bytes_to_i16(got_pcm);
        let mut jb = JitterBuffer::with_defaults();
        jb.push(0, mono.clone());
        jb.push(1, mono.clone()); // second frame so jitter starts
        let out = jb.pop().expect("playout");
        assert_eq!(out, mono);

        // Spatial path produces finite stereo samples in range.
        let stereo = apply_gain_pan(&out, 0.8, -0.3);
        assert_eq!(stereo.len(), out.len() * 2);
        for s in &stereo {
            assert!(s.is_finite());
            assert!(*s >= -1.0 && *s <= 1.0);
        }
    }

    #[test]
    fn frame_assembler_emits_120ms_at_16k() {
        let mut fa = FrameAssembler::new(VOICE_SAMPLE_RATE);
        // Exactly one frame of silence.
        let input = vec![0.0f32; FRAME_SAMPLES];
        let frames = fa.push_f32(&input);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), FRAME_BYTES);
    }

    #[test]
    fn apply_gain_pan_center_and_mute() {
        let mono = vec![16_000i16, -16_000];
        let silent = apply_gain_pan(&mono, 0.0, 0.0);
        assert!(silent.iter().all(|&s| s.abs() < 1e-6));
        let center = apply_gain_pan(&mono, 1.0, 0.0);
        // Equal-power center: L ≈ R ≈ 0.707 * sample
        assert!((center[0] - center[1]).abs() < 1e-5);
    }
}
