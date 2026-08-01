/**
 * The reel's score, synthesised in the page.
 *
 * Follows the kit in `video/scripts/make-film-sfx.mjs` rather than inventing a
 * second sound language: equal-tempered A440, an A-minor pentatonic, tones that
 * are struck and decay rather than samples. Nothing is shipped as an asset —
 * the same reason the app synthesises its own start/stop cues in `sound.rs`.
 *
 * Two rules carried over from that kit, both deliberate:
 *
 * **The scale means something.** The film's "Accumulate" descends because
 * thoughts are being lost. This is the opposite — words are being gathered onto
 * a page — so it climbs: A3 C4 D4 E4 G4 A4 C5 D5. Nobody will consciously hear
 * the pentatonic. Everybody will hear that it keeps rising.
 *
 * **It is mixed to be ignorable.** A reel plays muted more often than not, so
 * the sound is a bonus for whoever has it on and never something the picture
 * depends on. `MASTER` is set so the sum peaks around -9 dBFS.
 */

/** Equal-tempered, A440. Ascending A-minor pentatonic across two octaves. */
const CLIMB = [220.0, 261.63, 293.66, 329.63, 392.0, 440.0, 523.25, 587.33];

/** The chord the card settles on: A minor, voiced low and open. */
const RESOLVE = [220.0, 261.63, 329.63, 440.0];

const MASTER = 0.5;

export type Score = {
  /** The stream to record alongside the canvas. */
  track: MediaStreamTrack;
  /** A word landed — climbs the scale, then ticks once it runs out. */
  word: (index: number) => void;
  /** The rule drawing in under the header. */
  sweep: () => void;
  /** The mark assembling: the chord, and the piece's resolution. */
  resolve: () => void;
  close: () => Promise<void>;
};

/**
 * A struck tone: a sine with a little odd harmonic, and an exponential decay.
 *
 * Percussive rather than sustained, so twenty-five of them in three seconds
 * read as a sequence of events instead of a drone.
 */
function strike(
  ctx: AudioContext,
  to: AudioNode,
  freq: number,
  at: number,
  gain: number,
  decay = 0.9,
) {
  const osc = ctx.createOscillator();
  const third = ctx.createOscillator();
  const env = ctx.createGain();

  osc.type = "sine";
  osc.frequency.value = freq;
  // A quiet octave above gives the tone an edge through a phone speaker,
  // where the fundamental alone is mostly inaudible.
  third.type = "sine";
  third.frequency.value = freq * 2;

  const edge = ctx.createGain();
  edge.gain.value = 0.18;
  third.connect(edge).connect(env);
  osc.connect(env);

  env.gain.setValueAtTime(0, at);
  // 8 ms attack: fast enough to feel struck, slow enough not to click.
  env.gain.linearRampToValueAtTime(gain * MASTER, at + 0.008);
  env.gain.exponentialRampToValueAtTime(0.0001, at + decay);
  env.connect(to);

  osc.start(at);
  third.start(at);
  osc.stop(at + decay + 0.05);
  third.stop(at + decay + 0.05);
}

/** Filtered noise — the rule drawing in, and the only non-tonal sound here. */
function sweep(ctx: AudioContext, to: AudioNode, at: number, gain: number) {
  const len = 0.45;
  const buf = ctx.createBuffer(1, Math.ceil(ctx.sampleRate * len), ctx.sampleRate);
  const d = buf.getChannelData(0);
  // Deterministic rather than Math.random, matching the film kit's prng rule:
  // the same reel should sound the same twice.
  let seed = 1337;
  for (let i = 0; i < d.length; i++) {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    d[i] = (seed / 0xffffffff) * 2 - 1;
  }
  const src = ctx.createBufferSource();
  src.buffer = buf;

  const band = ctx.createBiquadFilter();
  band.type = "bandpass";
  band.frequency.setValueAtTime(700, at);
  band.frequency.exponentialRampToValueAtTime(2600, at + len);
  band.Q.value = 1.2;

  const env = ctx.createGain();
  env.gain.setValueAtTime(0, at);
  env.gain.linearRampToValueAtTime(gain * MASTER, at + 0.06);
  env.gain.exponentialRampToValueAtTime(0.0001, at + len);

  src.connect(band).connect(env).connect(to);
  src.start(at);
  src.stop(at + len);
}

/**
 * Open an audio context wired to a recordable stream.
 *
 * Returns `null` when the browser has no Web Audio — the reel is still worth
 * making silent, so the caller carries on rather than failing.
 */
export function openScore(): Score | null {
  const Ctx: typeof AudioContext | undefined =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext })
      .webkitAudioContext;
  if (!Ctx) return null;

  const ctx = new Ctx();
  const dest = ctx.createMediaStreamDestination();

  // A gentle low-pass over everything: phone speakers do nothing useful with
  // the top end, and it keeps the kit from sounding brittle after compression.
  const bus = ctx.createBiquadFilter();
  bus.type = "lowpass";
  bus.frequency.value = 7000;
  bus.connect(dest);

  const track = dest.stream.getAudioTracks()[0];
  if (!track) return null;

  return {
    track,
    word: (i) => {
      const now = ctx.currentTime;
      if (i < CLIMB.length) {
        // The headline words carry the melody, and get the level to match.
        strike(ctx, bus, CLIMB[i], now, 0.22 - i * 0.012, 1.1);
      } else {
        // Everything after is a tick on the top note, dropping away — present
        // enough to feel like arrivals, quiet enough not to become a rhythm.
        strike(ctx, bus, CLIMB[CLIMB.length - 1] * 2, now, 0.045, 0.12);
      }
    },
    sweep: () => sweep(ctx, bus, ctx.currentTime, 0.1),
    resolve: () => {
      const now = ctx.currentTime;
      // Rolled, not struck together: 40 ms apart reads as a chord arriving
      // rather than a stab, and lands on A4 — the pitch the climb started an
      // octave below.
      RESOLVE.forEach((f, i) => strike(ctx, bus, f, now + i * 0.04, 0.16, 2.6));
    },
    close: async () => {
      track.stop();
      await ctx.close().catch(() => {});
    },
  };
}
