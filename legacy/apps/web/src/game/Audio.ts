// Zero-asset retro audio, layered like real arcade hardware: every sound is
// synthesized from oscillators + one shared white-noise buffer (0 bytes of
// assets, spec: small bundle). A master compressor keeps layers from clipping
// and a touch of random pitch keeps repeats from sounding machine-stamped.
//
// Sound is ALWAYS on — there is no in-app mute. Players who want silence use
// the phone's ring/silent switch, which also keeps proximity behaviour
// consistent. We deliberately do NOT touch navigator.audioSession, so the OS
// (incl. older iOS) controls muting via that hardware switch.

interface ToneOpts {
  freq: number;
  end?: number; // exponential pitch sweep target
  type?: OscillatorType;
  dur: number;
  gain?: number;
  at?: number; // delay (s)
  atAbs?: number; // absolute audio-clock time (music scheduling)
  pan?: number; // -1..1
  music?: boolean; // route through the quieter music submix
  exact?: boolean; // no pitch jitter (music notes must stay in tune)
}

interface BurstOpts {
  dur: number;
  gain?: number;
  filter?: BiquadFilterType;
  freq?: number; // filter cutoff/center
  freqEnd?: number;
  q?: number;
  at?: number;
  atAbs?: number;
  pan?: number;
  music?: boolean;
}

export class Sfx {
  private ctx: AudioContext | null = null;
  private out!: GainNode;
  private musicOut!: GainNode; // music sits UNDER the gameplay sounds
  private noiseBuf: AudioBuffer | null = null;
  private musicTimer: ReturnType<typeof setInterval> | null = null;
  private musicNext = 0;
  private musicStep = 0;

  private ensure(): AudioContext {
    if (!this.ctx) {
      this.ctx = new AudioContext();
      const comp = this.ctx.createDynamicsCompressor();
      comp.threshold.value = -18;
      comp.knee.value = 24;
      comp.ratio.value = 6;
      this.out = this.ctx.createGain();
      this.out.gain.value = 1;
      this.out.connect(comp);
      comp.connect(this.ctx.destination);
      this.musicOut = this.ctx.createGain();
      this.musicOut.gain.value = 0.5; // ~half the sfx level
      this.musicOut.connect(this.out);
    }
    if (this.ctx.state === "suspended") void this.ctx.resume();
    return this.ctx;
  }

  /** Create + resume the context from a real user gesture. Mobile browsers
   *  (iOS especially, including OLD versions) only unlock audio inside a
   *  tap/click handler, so call this once on the first interaction — before any
   *  game-event sound, which fires outside a gesture and otherwise can't unlock
   *  a suspended context. The zero-length silent buffer below is the canonical
   *  primer that older iOS Safari needs to actually start the audio hardware. */
  unlock(): void {
    this.safe(() => {
      const ctx = this.ensure();
      const src = ctx.createBufferSource();
      src.buffer = ctx.createBuffer(1, 1, ctx.sampleRate);
      src.connect(ctx.destination);
      src.start(0);
    });
  }

  /** ±6% pitch variation — repeats don't sound machine-stamped. */
  private static jitter(f: number): number {
    return f * (0.94 + Math.random() * 0.12);
  }

  private route(node: AudioNode, pan: number | undefined, music = false): void {
    const dest = music ? this.musicOut : this.out;
    if (pan !== undefined && "createStereoPanner" in this.ctx!) {
      const p = this.ctx!.createStereoPanner();
      p.pan.value = Math.max(-1, Math.min(1, pan));
      node.connect(p);
      p.connect(dest);
    } else {
      node.connect(dest);
    }
  }

  private tone(o: ToneOpts): void {
    const ctx = this.ensure();
    const t0 = o.atAbs ?? ctx.currentTime + (o.at ?? 0);
    const osc = ctx.createOscillator();
    osc.type = o.type ?? "square";
    osc.frequency.setValueAtTime(o.exact ? o.freq : Sfx.jitter(o.freq), t0);
    if (o.end) osc.frequency.exponentialRampToValueAtTime(Math.max(20, o.end), t0 + o.dur);
    const g = ctx.createGain();
    g.gain.setValueAtTime(o.gain ?? 0.06, t0);
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + o.dur);
    osc.connect(g);
    this.route(g, o.pan, o.music);
    osc.start(t0);
    osc.stop(t0 + o.dur);
  }

  private burst(o: BurstOpts): void {
    const ctx = this.ensure();
    if (!this.noiseBuf) {
      this.noiseBuf = ctx.createBuffer(1, ctx.sampleRate * 0.5, ctx.sampleRate);
      const d = this.noiseBuf.getChannelData(0);
      for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1;
    }
    const t0 = o.atAbs ?? ctx.currentTime + (o.at ?? 0);
    const src = ctx.createBufferSource();
    src.buffer = this.noiseBuf;
    src.playbackRate.value = 0.85 + Math.random() * 0.3;
    const f = ctx.createBiquadFilter();
    f.type = o.filter ?? "bandpass";
    f.frequency.setValueAtTime(Sfx.jitter(o.freq ?? 1800), t0);
    if (o.freqEnd) f.frequency.exponentialRampToValueAtTime(o.freqEnd, t0 + o.dur);
    f.Q.value = o.q ?? 0.9;
    const g = ctx.createGain();
    g.gain.setValueAtTime(o.gain ?? 0.1, t0);
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + o.dur);
    src.connect(f);
    f.connect(g);
    this.route(g, o.pan, o.music);
    src.start(t0);
    src.stop(t0 + o.dur);
  }

  /** Wrap every public cue — audio is decoration; never crash the game. */
  private safe(fn: () => void): void {
    try {
      fn();
    } catch {
      /* no audio — fine */
    }
  }

  // ── combat ──
  /** Own gunshot: noise crack + pitch-swept snap + low thump. */
  shoot(): void {
    this.safe(() => {
      this.burst({ dur: 0.09, gain: 0.14, filter: "bandpass", freq: 2400, freqEnd: 700, q: 0.7 });
      this.tone({ freq: 1300, end: 180, type: "square", dur: 0.08, gain: 0.045 });
      this.tone({ freq: 150, end: 55, type: "sine", dur: 0.12, gain: 0.08 });
    });
  }

  /** Opponent's gunshot: duller, quieter, panned toward where it came from. */
  enemyShoot(pan: number): void {
    this.safe(() => {
      this.burst({ dur: 0.1, gain: 0.075, filter: "lowpass", freq: 1000, q: 0.6, pan });
      this.tone({ freq: 600, end: 130, type: "square", dur: 0.09, gain: 0.022, pan });
    });
  }

  /** Hit marker — short bright tick (you connected). */
  hit(): void {
    this.safe(() => {
      this.tone({ freq: 1900, end: 1400, type: "square", dur: 0.05, gain: 0.05 });
      this.burst({ dur: 0.04, gain: 0.04, filter: "highpass", freq: 3200 });
    });
  }

  /** You got shot — low thump + muffled noise. */
  hurt(): void {
    this.safe(() => {
      this.tone({ freq: 130, end: 55, type: "sawtooth", dur: 0.16, gain: 0.09 });
      this.burst({ dur: 0.12, gain: 0.06, filter: "lowpass", freq: 500 });
    });
  }

  /** Frag confirmed — rising 3-note arpeggio + noise tail. */
  kill(): void {
    this.safe(() => {
      this.tone({ freq: 523, dur: 0.09, gain: 0.07 });
      this.tone({ freq: 659, dur: 0.09, gain: 0.07, at: 0.08 });
      this.tone({ freq: 880, end: 1046, dur: 0.14, gain: 0.08, at: 0.16 });
      this.burst({ dur: 0.2, gain: 0.04, filter: "highpass", freq: 2400, at: 0.16 });
    });
  }

  /** You died — descending sweep. */
  death(): void {
    this.safe(() => {
      this.tone({ freq: 420, end: 60, type: "sawtooth", dur: 0.5, gain: 0.08 });
      this.burst({ dur: 0.35, gain: 0.05, filter: "lowpass", freq: 700, freqEnd: 120 });
    });
  }

  /** Barrel explosion — deliberately restrained: distance-attenuated (a boom
   *  across the map is a faraway thud), panned toward the blast, and a chain
   *  plays as ONE boom with a longer tail instead of N overlapping copies. */
  explosion(pan: number, dist: number, chain = 1): void {
    this.safe(() => {
      const a = Math.max(0.2, 1 - dist / 55); // distance attenuation
      const tail = Math.min(0.85, 0.4 + chain * 0.12);
      this.burst({ dur: tail, gain: 0.15 * a, filter: "lowpass", freq: 850, freqEnd: 110, q: 0.5, pan });
      this.tone({ freq: 85, end: 28, type: "sine", dur: 0.45, gain: 0.11 * a, pan });
      this.burst({ dur: 0.16, gain: 0.06 * a, filter: "bandpass", freq: 2600, at: 0.03, pan }); // debris crackle
    });
  }

  /** Health pickup — soft chime pair with a quick echo. */
  heal(): void {
    this.safe(() => {
      this.tone({ freq: 660, type: "triangle", dur: 0.09, gain: 0.06 });
      this.tone({ freq: 990, type: "triangle", dur: 0.12, gain: 0.06, at: 0.07 });
      this.tone({ freq: 990, type: "triangle", dur: 0.1, gain: 0.025, at: 0.21 }); // echo
    });
  }

  /** Landed after a fall — subtle thud. */
  land(): void {
    this.safe(() => {
      this.tone({ freq: 95, end: 50, type: "sine", dur: 0.08, gain: 0.045 });
    });
  }

  // ── match flow ──
  /** One countdown beep per second ("3… 2… 1…"). */
  countdown(): void {
    this.safe(() => this.tone({ freq: 440, type: "square", dur: 0.09, gain: 0.05 }));
  }

  /** GO — the match-start horn. */
  startHorn(): void {
    this.safe(() => {
      this.tone({ freq: 392, dur: 0.1, gain: 0.07 });
      this.tone({ freq: 523, dur: 0.1, gain: 0.07, at: 0.09 });
      this.tone({ freq: 784, end: 880, dur: 0.28, gain: 0.085, at: 0.18 });
    });
  }

  // ── music (zero-asset step sequencer on the quieter music submix) ──

  /** Clutch time: tense minor bass pulse + ticking hat when the clock runs
   *  out. `fast` = sudden death (replaces the old one-shot sting). Uses the
   *  standard lookahead pattern: a coarse interval schedules notes a beat
   *  ahead on the sample-accurate audio clock. */
  startClutch(fast: boolean): void {
    this.safe(() => {
      const ctx = this.ensure();
      this.stopMusic();
      const bpm = fast ? 172 : 138;
      const stepDur = 60 / bpm / 2; // eighth notes
      // A-minor pulse — root with darker color tones, tension not melody
      const bass = [55, 0, 55, 0, 65.4, 0, 55, 0, 55, 0, 73.4, 0, 65.4, 0, 49, 0];
      this.musicNext = ctx.currentTime + 0.06;
      this.musicStep = 0;
      this.musicTimer = setInterval(() => {
        while (this.musicNext < ctx.currentTime + 0.22) {
          const f = bass[this.musicStep % bass.length];
          if (f > 0) {
            this.tone({ freq: f, type: "triangle", dur: stepDur * 0.85, gain: 0.06, atAbs: this.musicNext, music: true, exact: true });
            this.tone({ freq: f * 2, type: "square", dur: stepDur * 0.4, gain: 0.018, atAbs: this.musicNext, music: true, exact: true });
          }
          if (this.musicStep % 4 === 2) {
            this.burst({ dur: 0.03, gain: 0.02, filter: "highpass", freq: 6500, atAbs: this.musicNext, music: true }); // tick
          }
          this.musicNext += stepDur;
          this.musicStep += 1;
        }
      }, 100);
    });
  }

  stopMusic(): void {
    if (this.musicTimer) {
      clearInterval(this.musicTimer);
      this.musicTimer = null;
    }
  }

  /** Winner takes the pot — a short major fanfare, then silence (no loop
   *  while staring at the end screen). */
  victory(): void {
    this.safe(() => {
      this.stopMusic();
      // [freq, start, dur] — C-major riff climbing to a held high G
      const riff: [number, number, number][] = [
        [523, 0, 0.12], [659, 0.12, 0.12], [784, 0.24, 0.12], [1046, 0.36, 0.24],
        [784, 0.64, 0.12], [1046, 0.76, 0.12], [1318, 0.88, 0.32],
        [1046, 1.28, 0.12], [1318, 1.4, 0.12], [1568, 1.52, 0.6],
      ];
      for (const [f, at, dur] of riff) {
        this.tone({ freq: f, type: "square", dur, gain: 0.06, at, music: true, exact: true });
        this.tone({ freq: f / 2, type: "triangle", dur, gain: 0.04, at, music: true, exact: true });
      }
      this.burst({ dur: 0.5, gain: 0.025, filter: "highpass", freq: 5000, at: 1.52, music: true }); // sparkle
    });
  }

  /** Lost the duel — a quieter, slower minor cadence. */
  defeat(): void {
    this.safe(() => {
      this.stopMusic();
      const cadence: [number, number, number][] = [
        [440, 0, 0.3], [349, 0.32, 0.3], [330, 0.64, 0.3], [294, 0.96, 0.5],
      ];
      for (const [f, at, dur] of cadence) {
        this.tone({ freq: f, type: "triangle", dur, gain: 0.045, at, music: true, exact: true });
        this.tone({ freq: f / 2, type: "sine", dur: dur * 1.2, gain: 0.035, at, music: true, exact: true });
      }
      this.tone({ freq: 110, end: 65, type: "sine", dur: 0.8, gain: 0.04, at: 1.0, music: true });
    });
  }

  /** Tiny UI click for menu buttons. */
  uiClick(): void {
    this.safe(() => this.tone({ freq: 1200, end: 900, type: "square", dur: 0.03, gain: 0.025 }));
  }
}
