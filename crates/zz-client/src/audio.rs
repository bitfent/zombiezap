//! Zero-asset synthesized SFX (the ShotAnte oscillator philosophy): sole
//! consumer of `seams::SfxQueue`. Generates short mono f32 buffers at startup
//! and plays them via a custom Bevy `Decodable` source — no audio files, no
//! extra crates beyond the `bevy_audio` feature.

use std::num::NonZeroU16;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use bevy::audio::{AddAudioSource, Decodable, Source, Volume};
use bevy::prelude::*;
use bevy::reflect::TypePath;

use crate::seams::{Sfx, SfxQueue};

const SAMPLE_RATE: u32 = 44_100;
const MAX_CONCURRENT: usize = 8;

/// Soft priority: lower = first to drop when the voice pool is saturated.
/// Remote shoots are the first casualty; combat feedback stays.
fn sfx_priority(sfx: &Sfx) -> u8 {
    match sfx {
        Sfx::Shoot { from_me: false } => 0,
        Sfx::Growl { .. } => 1,
        Sfx::Click | Sfx::DryClick => 2,
        Sfx::ReloadClack => 3,
        Sfx::MeleeSwing => 3,
        Sfx::Shoot { from_me: true } => 4,
        Sfx::MeleeHit => 4,
        Sfx::HitConfirm => 5,
        Sfx::Pickup => 6,
        Sfx::Hurt => 7,
        Sfx::KillConfirm { .. } => 8,
        Sfx::Explosion { .. } => 9,
        Sfx::TeamWipe => 10,
    }
}

/// Hard cap on concurrent growl voices (~6); combat SFX still use the pool.
const MAX_GROWLS: usize = 6;

// ---------------------------------------------------------------------------
// Custom Decodable: pre-baked mono f32 samples
// ---------------------------------------------------------------------------

/// Reusable one-shot clip of mono f32 samples at [`SAMPLE_RATE`].
#[derive(Asset, TypePath, Clone)]
struct SynthClip {
    samples: Arc<[f32]>,
}

struct SynthDecoder {
    samples: Arc<[f32]>,
    index: usize,
    sample_rate: NonZeroU32,
    channels: NonZeroU16,
}

impl SynthDecoder {
    fn new(samples: Arc<[f32]>) -> Self {
        Self {
            samples,
            index: 0,
            sample_rate: NonZeroU32::new(SAMPLE_RATE).expect("SAMPLE_RATE > 0"),
            channels: NonZeroU16::new(1).expect("mono"),
        }
    }
}

impl Iterator for SynthDecoder {
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

impl Source for SynthDecoder {
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
        let secs = self.samples.len() as f64 / SAMPLE_RATE as f64;
        Some(Duration::from_secs_f64(secs))
    }
}

impl Decodable for SynthClip {
    type Decoder = SynthDecoder;

    fn decoder(&self) -> Self::Decoder {
        SynthDecoder::new(Arc::clone(&self.samples))
    }
}

// ---------------------------------------------------------------------------
// Bank of pre-generated handles
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct SfxBank {
    shoot: Handle<SynthClip>,
    hit_confirm: Handle<SynthClip>,
    kill: Handle<SynthClip>,
    kill_headshot: Handle<SynthClip>,
    explosion: Handle<SynthClip>,
    hurt: Handle<SynthClip>,
    pickup: Handle<SynthClip>,
    team_wipe: Handle<SynthClip>,
    click: Handle<SynthClip>,
    dry_click: Handle<SynthClip>,
    melee_swing: Handle<SynthClip>,
    melee_hit: Handle<SynthClip>,
    reload_clack: Handle<SynthClip>,
    growl: Handle<SynthClip>,
}

/// Marks a spawned one-shot SFX voice for concurrent-count queries.
#[derive(Component)]
struct SfxVoice;

/// Marker for growl voices so we can enforce a ~6 concurrent cap.
#[derive(Component)]
struct GrowlVoice;

pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_audio_source::<SynthClip>()
            .add_systems(Startup, setup_sfx_bank)
            .add_systems(Update, drain_sfx_queue);
    }
}

fn setup_sfx_bank(mut commands: Commands, mut clips: ResMut<Assets<SynthClip>>) {
    let bank = SfxBank {
        shoot: clips.add(SynthClip {
            samples: Arc::from(synth_shoot()),
        }),
        hit_confirm: clips.add(SynthClip {
            samples: Arc::from(synth_hit_confirm()),
        }),
        kill: clips.add(SynthClip {
            samples: Arc::from(synth_kill_confirm(false)),
        }),
        kill_headshot: clips.add(SynthClip {
            samples: Arc::from(synth_kill_confirm(true)),
        }),
        explosion: clips.add(SynthClip {
            samples: Arc::from(synth_explosion()),
        }),
        hurt: clips.add(SynthClip {
            samples: Arc::from(synth_hurt()),
        }),
        pickup: clips.add(SynthClip {
            samples: Arc::from(synth_pickup()),
        }),
        team_wipe: clips.add(SynthClip {
            samples: Arc::from(synth_team_wipe()),
        }),
        click: clips.add(SynthClip {
            samples: Arc::from(synth_click()),
        }),
        dry_click: clips.add(SynthClip {
            samples: Arc::from(synth_dry_click()),
        }),
        melee_swing: clips.add(SynthClip {
            samples: Arc::from(synth_melee_whoosh()),
        }),
        melee_hit: clips.add(SynthClip {
            samples: Arc::from(synth_melee_thunk()),
        }),
        reload_clack: clips.add(SynthClip {
            samples: Arc::from(synth_reload_clack()),
        }),
        growl: clips.add(SynthClip {
            samples: Arc::from(synth_growl()),
        }),
    };
    commands.insert_resource(bank);
}

fn drain_sfx_queue(
    mut queue: ResMut<SfxQueue>,
    bank: Res<SfxBank>,
    voices: Query<&SfxVoice>,
    growls: Query<&GrowlVoice>,
    mut commands: Commands,
) {
    let mut active = voices.iter().count();
    let mut growl_active = growls.iter().count();

    while let Some(sfx) = queue.0.pop_front() {
        let priority = sfx_priority(&sfx);
        let is_growl = matches!(sfx, Sfx::Growl { .. });

        // Cap concurrent voices. Queue is always drained (sole consumer) so
        // saturated frames drop events instead of backlog-ing. Lowest
        // priority (remote shoots = 0, then growls) is skipped first when
        // saturated; everything is hard-capped at MAX_CONCURRENT.
        if active >= MAX_CONCURRENT {
            continue;
        }
        // Growls also hard-cap at ~6 concurrent voices.
        if is_growl && growl_active >= MAX_GROWLS {
            continue;
        }
        // Under mild pressure (near cap), still drop remote shoots first.
        if active + 1 >= MAX_CONCURRENT && priority == 0 {
            continue;
        }

        let (handle, volume) = match &sfx {
            Sfx::Shoot { from_me } => {
                let vol = if *from_me { 1.0 } else { 0.4 };
                (bank.shoot.clone(), vol)
            }
            Sfx::HitConfirm => (bank.hit_confirm.clone(), 1.0),
            Sfx::KillConfirm { headshot } => {
                if *headshot {
                    (bank.kill_headshot.clone(), 1.0)
                } else {
                    (bank.kill.clone(), 1.0)
                }
            }
            Sfx::Explosion { dist } => {
                let vol = (1.0 - dist / 40.0).clamp(0.05, 1.0);
                (bank.explosion.clone(), vol)
            }
            Sfx::Hurt => (bank.hurt.clone(), 1.0),
            Sfx::Pickup => (bank.pickup.clone(), 1.0),
            Sfx::TeamWipe => (bank.team_wipe.clone(), 1.0),
            Sfx::Click => (bank.click.clone(), 0.7),
            Sfx::DryClick => (bank.dry_click.clone(), 0.85),
            Sfx::MeleeSwing => (bank.melee_swing.clone(), 0.9),
            Sfx::MeleeHit => (bank.melee_hit.clone(), 1.0),
            Sfx::ReloadClack => (bank.reload_clack.clone(), 0.85),
            Sfx::Growl { volume } => (bank.growl.clone(), volume.clamp(0.05, 0.85)),
        };

        if is_growl {
            commands.spawn((
                AudioPlayer(handle),
                PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume)),
                SfxVoice,
                GrowlVoice,
            ));
            growl_active += 1;
        } else {
            commands.spawn((
                AudioPlayer(handle),
                PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume)),
                SfxVoice,
            ));
        }
        active += 1;
    }
}

// ---------------------------------------------------------------------------
// Synth primitives (plain math + tiny xorshift — no rand)
// ---------------------------------------------------------------------------

/// Deterministic white noise via xorshift32, range ≈ [-1, 1].
struct XorShift32(u32);

impl XorShift32 {
    fn new(seed: u32) -> Self {
        // Zero state is degenerate; bump to a non-zero seed.
        Self(if seed == 0 { 0xA5A5_5A5A } else { seed })
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    fn next_f32(&mut self) -> f32 {
        // Map full u32 to [-1, 1].
        (self.next_u32() as f32 / (u32::MAX as f32 / 2.0)) - 1.0
    }
}

fn samples_for_ms(ms: f32) -> usize {
    ((SAMPLE_RATE as f32) * ms / 1000.0).round() as usize
}

fn square(phase: f32) -> f32 {
    if (phase % 1.0) < 0.5 { 1.0 } else { -1.0 }
}

fn sine(phase: f32) -> f32 {
    (phase * std::f32::consts::TAU).sin()
}

/// Peak-normalize / soft-clamp so |sample| ≤ 1.0.
fn finalize(mut buf: Vec<f32>) -> Vec<f32> {
    let mut peak = 0.0_f32;
    for s in &buf {
        peak = peak.max(s.abs());
    }
    if peak > 1.0 {
        let inv = 1.0 / peak;
        for s in &mut buf {
            *s *= inv;
        }
    }
    // Guard against any remaining out-of-range / NaN from bad math.
    for s in &mut buf {
        if !s.is_finite() {
            *s = 0.0;
        } else {
            *s = s.clamp(-1.0, 1.0);
        }
    }
    buf
}

/// 60 ms square-wave blip (150→90 Hz sweep) + white-noise snap.
pub(crate) fn synth_shoot() -> Vec<f32> {
    let n = samples_for_ms(60.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0xC0FF_EE01);
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        let freq = 150.0 + (90.0 - 150.0) * u;
        let phase = t * freq;
        let env = (1.0 - u).powf(1.5);
        let tone = square(phase) * 0.45 * env;
        // Noise snap: first ~8 ms.
        let snap_env = if u < 0.15 {
            (1.0 - u / 0.15).powf(2.0)
        } else {
            0.0
        };
        let snap = noise.next_f32() * 0.55 * snap_env;
        *s = tone + snap;
    }
    finalize(buf)
}

/// 30 ms 1.2 kHz tick.
pub(crate) fn synth_hit_confirm() -> Vec<f32> {
    let n = samples_for_ms(30.0);
    let mut buf = vec![0.0; n];
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        let env = (1.0 - u).powf(2.0);
        *s = sine(t * 1200.0) * 0.55 * env;
    }
    finalize(buf)
}

/// Two rising square notes; headshot a perfect fifth higher.
pub(crate) fn synth_kill_confirm(headshot: bool) -> Vec<f32> {
    let note_ms = 70.0;
    let gap_ms = 20.0;
    let n1 = samples_for_ms(note_ms);
    let gap = samples_for_ms(gap_ms);
    let n2 = samples_for_ms(note_ms);
    let total = n1 + gap + n2;
    let mut buf = vec![0.0; total];

    let base = if headshot { 440.0 * 1.5 } else { 440.0 }; // fifth up
    let f1 = base;
    let f2 = base * (4.0 / 3.0); // rising fourth

    for (i, s) in buf.iter_mut().take(n1).enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n1 as f32;
        let env = if u < 0.1 { u / 0.1 } else { (1.0 - u).max(0.0) };
        *s = square(t * f1) * 0.4 * env;
    }
    let note2 = &mut buf[n1 + gap..n1 + gap + n2];
    for (i, s) in note2.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n2 as f32;
        let env = if u < 0.1 { u / 0.1 } else { (1.0 - u).max(0.0) };
        *s = square(t * f2) * 0.45 * env;
    }
    finalize(buf)
}

/// 350 ms low-pass-ish filtered noise with exponential decay.
pub(crate) fn synth_explosion() -> Vec<f32> {
    let n = samples_for_ms(350.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0xB00B_4501);
    // One-pole low-pass state.
    let mut lp = 0.0_f32;
    let alpha = 0.08_f32;
    for (i, s) in buf.iter_mut().enumerate() {
        let u = i as f32 / n as f32;
        let env = (-6.0 * u).exp(); // exponential decay
        let white = noise.next_f32();
        lp += alpha * (white - lp);
        *s = lp * 0.95 * env;
    }
    finalize(buf)
}

/// 90 ms low sine thud (110 Hz) + slight noise.
pub(crate) fn synth_hurt() -> Vec<f32> {
    let n = samples_for_ms(90.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0x0BAD_C0DE);
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        let env = (1.0 - u).powf(1.8);
        let thud = sine(t * 110.0) * 0.7 * env;
        let grit = noise.next_f32() * 0.12 * env;
        *s = thud + grit;
    }
    finalize(buf)
}

/// 120 ms rising chirp (400→900 Hz sine).
pub(crate) fn synth_pickup() -> Vec<f32> {
    let n = samples_for_ms(120.0);
    let mut buf = vec![0.0; n];
    let mut phase = 0.0_f32;
    for (i, s) in buf.iter_mut().enumerate() {
        let u = i as f32 / n as f32;
        let freq = 400.0 + (900.0 - 400.0) * u;
        phase += freq / SAMPLE_RATE as f32;
        let env = if u < 0.1 {
            u / 0.1
        } else {
            (1.0 - (u - 0.1) / 0.9).max(0.0).powf(0.7)
        };
        *s = sine(phase) * 0.5 * env;
    }
    finalize(buf)
}

/// ~600 ms descending three-note minor phrase (allowed to exceed 400 ms).
pub(crate) fn synth_team_wipe() -> Vec<f32> {
    // Minor triad descending: A4 → F4 → D4 (approx).
    let notes = [440.0_f32, 349.23, 293.66];
    let note_ms = 160.0;
    let gap_ms = 40.0;
    let n_note = samples_for_ms(note_ms);
    let n_gap = samples_for_ms(gap_ms);
    let total = notes.len() * n_note + (notes.len() - 1) * n_gap;
    let mut buf = vec![0.0; total];
    let mut cursor = 0;
    for (ni, &freq) in notes.iter().enumerate() {
        let note = &mut buf[cursor..cursor + n_note];
        for (i, s) in note.iter_mut().enumerate() {
            let t = i as f32 / SAMPLE_RATE as f32;
            let u = i as f32 / n_note as f32;
            let attack = if u < 0.08 { u / 0.08 } else { 1.0 };
            let release = (1.0 - u).powf(1.2);
            let env = attack * release;
            *s = square(t * freq) * 0.35 * env;
        }
        cursor += n_note;
        if ni + 1 < notes.len() {
            cursor += n_gap;
        }
    }
    finalize(buf)
}

/// 20 ms tick.
pub(crate) fn synth_click() -> Vec<f32> {
    let n = samples_for_ms(20.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0xC1C1_C1C1);
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        let env = (1.0 - u).powf(3.0);
        *s = sine(t * 1800.0) * 0.35 * env + noise.next_f32() * 0.15 * env;
    }
    finalize(buf)
}

/// Short metallic dry-fire click (empty chamber).
pub(crate) fn synth_dry_click() -> Vec<f32> {
    let n = samples_for_ms(35.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0x0D41_F14E);
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        let env = (1.0 - u).powf(4.0);
        *s = sine(t * 2400.0) * 0.4 * env + noise.next_f32() * 0.25 * env;
    }
    finalize(buf)
}

/// Melee whoosh: filtered noise sweep ~90 ms.
pub(crate) fn synth_melee_whoosh() -> Vec<f32> {
    let n = samples_for_ms(90.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0x0E1E_E400);
    let mut lp = 0.0_f32;
    for (i, s) in buf.iter_mut().enumerate() {
        let u = i as f32 / n as f32;
        let env = if u < 0.2 {
            u / 0.2
        } else {
            (1.0 - (u - 0.2) / 0.8).max(0.0).powf(1.6)
        };
        let alpha = 0.04 + 0.2 * u; // brighten through the swing
        let white = noise.next_f32();
        lp += alpha * (white - lp);
        *s = lp * 0.85 * env;
    }
    finalize(buf)
}

/// Melee impact thunk: low sine + grit ~70 ms.
pub(crate) fn synth_melee_thunk() -> Vec<f32> {
    let n = samples_for_ms(70.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0x07B4_C001);
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        let env = (1.0 - u).powf(2.2);
        *s = sine(t * 90.0) * 0.75 * env + noise.next_f32() * 0.2 * env;
    }
    finalize(buf)
}

/// Reload magazine clack: short mid click ~40 ms.
pub(crate) fn synth_reload_clack() -> Vec<f32> {
    let n = samples_for_ms(40.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0x00E1_0AD0);
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        let env = (1.0 - u).powf(2.5);
        *s = sine(t * 900.0) * 0.45 * env + square(t * 600.0) * 0.2 * env + noise.next_f32() * 0.1 * env;
    }
    finalize(buf)
}

/// ~280 ms guttural zombie growl: low filtered noise + slow sine rumble.
pub(crate) fn synth_growl() -> Vec<f32> {
    let n = samples_for_ms(280.0);
    let mut buf = vec![0.0; n];
    let mut noise = XorShift32::new(0xDEAD_BEEF);
    let mut lp = 0.0_f32;
    let alpha = 0.06_f32;
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let u = i as f32 / n as f32;
        // Slow attack, long decay — animal snarl.
        let env = if u < 0.12 {
            u / 0.12
        } else {
            (1.0 - (u - 0.12) / 0.88).max(0.0).powf(1.4)
        };
        let white = noise.next_f32();
        lp += alpha * (white - lp);
        // Sub-rumble + formant-ish mid growl.
        let rumble = sine(t * 55.0) * 0.55 + sine(t * 90.0 + 0.3) * 0.25;
        let grit = lp * 0.7;
        // Mild amplitude modulation for a throaty pulse.
        let throat = 0.7 + 0.3 * sine(t * 7.0);
        *s = (rumble + grit) * 0.55 * env * throat;
    }
    finalize(buf)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_buffer_ok(buf: &[f32], min_ms: f32, max_ms: f32) {
        assert!(!buf.is_empty(), "buffer empty");
        let ms = buf.len() as f32 * 1000.0 / SAMPLE_RATE as f32;
        assert!(
            ms >= min_ms && ms <= max_ms,
            "duration {ms} ms outside [{min_ms}, {max_ms}]"
        );
        let mut peak = 0.0_f32;
        for (i, &s) in buf.iter().enumerate() {
            assert!(s.is_finite(), "NaN/Inf at sample {i}");
            peak = peak.max(s.abs());
        }
        assert!(peak <= 1.0 + 1e-5, "peak {peak} > 1.0");
        // Must have some energy so we didn't generate silence by accident.
        assert!(peak > 0.05, "peak {peak} too quiet — likely silent buffer");
    }

    #[test]
    fn shoot_length_and_bounds() {
        assert_buffer_ok(&synth_shoot(), 50.0, 70.0);
    }

    #[test]
    fn hit_confirm_length_and_bounds() {
        assert_buffer_ok(&synth_hit_confirm(), 20.0, 40.0);
    }

    #[test]
    fn kill_confirm_variants() {
        assert_buffer_ok(&synth_kill_confirm(false), 120.0, 200.0);
        assert_buffer_ok(&synth_kill_confirm(true), 120.0, 200.0);
    }

    #[test]
    fn explosion_length_and_bounds() {
        assert_buffer_ok(&synth_explosion(), 300.0, 400.0);
    }

    #[test]
    fn hurt_length_and_bounds() {
        assert_buffer_ok(&synth_hurt(), 70.0, 110.0);
    }

    #[test]
    fn pickup_length_and_bounds() {
        assert_buffer_ok(&synth_pickup(), 100.0, 140.0);
    }

    #[test]
    fn team_wipe_length_and_bounds() {
        // Allowed to exceed 400 ms; ~600 ms phrase.
        assert_buffer_ok(&synth_team_wipe(), 450.0, 700.0);
    }

    #[test]
    fn click_length_and_bounds() {
        assert_buffer_ok(&synth_click(), 10.0, 30.0);
    }

    #[test]
    fn growl_length_and_bounds() {
        assert_buffer_ok(&synth_growl(), 240.0, 320.0);
    }

    #[test]
    fn xorshift_not_all_zero() {
        let mut rng = XorShift32::new(1);
        let mut any_nonzero = false;
        for _ in 0..32 {
            if rng.next_f32().abs() > 1e-6 {
                any_nonzero = true;
                break;
            }
        }
        assert!(any_nonzero);
    }

    #[test]
    fn samples_for_ms_roundtrip() {
        assert_eq!(samples_for_ms(1000.0), SAMPLE_RATE as usize);
        assert_eq!(samples_for_ms(0.0), 0);
    }

    #[test]
    fn priority_remote_shoot_lowest() {
        assert!(
            sfx_priority(&Sfx::Shoot { from_me: false })
                < sfx_priority(&Sfx::Shoot { from_me: true })
        );
        assert!(sfx_priority(&Sfx::Shoot { from_me: false }) < sfx_priority(&Sfx::Hurt));
        assert!(sfx_priority(&Sfx::TeamWipe) > sfx_priority(&Sfx::Click));
    }
}
