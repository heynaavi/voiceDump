import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import gsap from "gsap";

import { BEAT, EASE } from "../lib/motion";

type State = "idle" | "recording" | "transcribing";

const BARS = 28;
/** How fast a bar falls back once the voice drops. */
const DECAY = 0.86;

/**
 * The global dictation overlay — its own always-on-top window, floating over
 * whatever app you're typing into.
 *
 * Levels are real: ffmpeg meters the capture on its way to disk and Rust
 * forwards the RMS, so this is the actual voice rather than a decorative
 * animation.
 *
 * This is the one surface that departs from §1's zero-radius rule. It isn't app
 * chrome — it sits on top of Chrome, Slack, anything — and a hard-edged black
 * rectangle over someone else's UI reads as an error dialog, not as a HUD.
 */
export function DictationOverlay() {
  // Defaults to "recording", not "idle". Rust only ever shows this window while
  // dictation is running, so the visible state is the correct default — and if
  // the state event were ever missed, an "idle" default would render nothing
  // and the window would show as an empty transparent rectangle, which is
  // indistinguishable from the overlay being broken.
  const [state, setState] = useState<State>("recording");
  const [elapsed, setElapsed] = useState(0);
  const shellRef = useRef<HTMLDivElement>(null);
  const barsRef = useRef<(HTMLSpanElement | null)[]>([]);
  const levels = useRef<number[]>(new Array(BARS).fill(0));
  const incoming = useRef(0);
  const started = useRef(0);

  useEffect(() => {
    const subs = [
      listen<State>("dictation-state", (e) => {
        setState(e.payload);
        if (e.payload === "recording") {
          started.current = Date.now();
          levels.current = new Array(BARS).fill(0);
        }
      }),
      listen<number>("dictation-level", (e) => {
        incoming.current = e.payload;
      }),
    ];
    return () => {
      subs.forEach((s) => s.then((un) => un()).catch(() => {}));
    };
  }, []);

  // Entrance. Scale from slightly small with a soft overshoot: V3 §7.2 gives
  // `back.out(1.7–1.9)` to "something arriving under its own weight — the
  // pill", and this is that pill. The one curve in the app that overshoots, and
  // named in the system rather than an exception to it.
  useEffect(() => {
    const el = shellRef.current;
    if (!el || state === "idle") return;
    gsap.fromTo(
      el,
      { opacity: 0, y: 14, scale: 0.94 },
      { opacity: 1, y: 0, scale: 1, duration: BEAT.reveal, ease: EASE.arrive },
    );
  }, [state]);

  // Bar animation. Heights are written straight to the DOM rather than through
  // React state: this runs every frame, and re-rendering 28 nodes per frame is
  // exactly the kind of work that makes an overlay feel heavy.
  useEffect(() => {
    if (state !== "recording") return;
    let raf = 0;
    const tick = () => {
      const next = levels.current.slice(1);
      next.push(incoming.current);
      // Everything ahead of the newest sample eases down, so speech leaves a
      // trailing wake instead of a hard edge.
      for (let i = 0; i < next.length - 1; i++) {
        next[i] = Math.max(next[i] * DECAY, 0);
      }
      levels.current = next;

      next.forEach((v, i) => {
        const bar = barsRef.current[i];
        if (!bar) return;
        const h = 3 + v * 21;
        bar.style.height = `${h}px`;
        bar.style.opacity = `${0.28 + v * 0.72}`;
      });

      setElapsed((Date.now() - started.current) / 1000);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [state]);

  // Idle still renders nothing, but Rust hides the window in that state anyway,
  // so this is belt-and-braces rather than the primary visibility control.
  if (state === "idle") return null;

  const recording = state === "recording";

  return (
    <div
      data-tauri-drag-region
      className="flex h-screen w-screen cursor-grab items-center justify-center active:cursor-grabbing"
    >
      <div
        ref={shellRef}
        className="overlay-pill flex items-center gap-3 px-4 py-2.5"
      >
        {recording ? (
          <>
            <span className="overlay-dot" />
            <div className="flex h-6 items-center gap-[2px]">
              {Array.from({ length: BARS }, (_, i) => (
                <span
                  key={i}
                  ref={(el) => {
                    barsRef.current[i] = el;
                  }}
                  className="block w-[2px] rounded-full bg-[#b8d4a4]"
                  style={{ height: 3, opacity: 0.3 }}
                />
              ))}
            </div>
            <span className="mono-data shrink-0 text-[11px] tabular-nums text-[#8fb07c]">
              {elapsed.toFixed(1)}s
            </span>
          </>
        ) : (
          <>
            <span className="overlay-spinner" />
            <span className="text-[12px] tracking-[-0.01em] text-[#d2e6c2]">
              Transcribing…
            </span>
          </>
        )}
      </div>
    </div>
  );
}
