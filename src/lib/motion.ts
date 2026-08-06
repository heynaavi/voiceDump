import { useEffect, useLayoutEffect, useRef, useState } from "react";
import gsap from "gsap";

/**
 * Motion vocabulary for the QWEE skin.
 *
 * The design language is machine-set and disciplined, so easing is snappy and
 * often *stepped* rather than springy — things click into place like a readout
 * updating, not like a bubble settling. Nothing bounces.
 */
export const EASE = {
  snap: "power4.out",
  step: "steps(4)",
  stepFine: "steps(6)",
  drift: "none",
} as const;

export function prefersReducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * Scoped GSAP context. Everything created inside is reverted on unmount, which
 * also restores the inline styles GSAP wrote — so `.gsap-init` elements never
 * get stranded at opacity 0.
 */
export function useGsap(
  setup: (ctx: { scope: HTMLElement }) => void,
  deps: unknown[] = [],
) {
  const scopeRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const scope = scopeRef.current;
    if (!scope) return;

    if (prefersReducedMotion()) {
      // Land everything at its resting state without animating.
      gsap.set(scope.querySelectorAll(".gsap-init"), { clearProps: "all", opacity: 1 });
      return;
    }

    // Do not start an entrance into a window nobody is looking at.
    //
    // GSAP's ticker is driven by requestAnimationFrame, which macOS does not
    // deliver to a hidden or fully occluded window. A `fromTo` that begins at
    // opacity 0 applies that instantly and then waits for a frame that never
    // arrives — so the content is not "animating in", it is invisible, and it
    // stays invisible until something happens to schedule a frame.
    //
    // Measured, in a webview reporting `visibilityState: "hidden"`: zero rAF
    // ticks in 400ms, and a walkthrough frozen at opacity 0 with its heading,
    // its body and its only button unreachable. On a screen that *is* the whole
    // window, that is not a missing flourish — it is a blank app.
    //
    // So the animation waits for the window to be seen. Anything mounted while
    // hidden simply appears, which is the correct behaviour anyway: an entrance
    // nobody watched has no reason to be replayed.
    let ctx: gsap.Context | null = null;
    let waiting: (() => void) | null = null;

    const run = () => {
      ctx = gsap.context(() => setup({ scope }), scope);
    };

    if (document.visibilityState === "hidden") {
      const onVisible = () => {
        if (document.visibilityState !== "hidden") {
          document.removeEventListener("visibilitychange", onVisible);
          waiting = null;
          run();
        }
      };
      document.addEventListener("visibilitychange", onVisible);
      waiting = () => document.removeEventListener("visibilitychange", onVisible);
    } else {
      run();
    }

    return () => {
      waiting?.();
      ctx?.revert();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  return scopeRef;
}

/**
 * A displayed progress value that always moves, even when the backend can't
 * report sub-steps.
 *
 * Whisper transcribes a chunk in one blocking call, so the real progress sits
 * pinned (e.g. at 10%) for the whole job and then jumps. This eases a shown
 * value toward the real one, and *while transcribing* lets it creep on toward a
 * ceiling so the bar reads as working rather than frozen. Early stages
 * (extract, load) track the real number honestly — no creep — and a real jump
 * ahead is always caught up to. `finished` snaps it home.
 */
export function useSmoothProgress(
  target: number,
  finished: boolean,
  transcribing: boolean,
): number {
  const [display, setDisplay] = useState(0);
  const s = useRef({ display: 0, target, finished, transcribing, last: 0, raf: 0 });
  s.current.target = target;
  s.current.finished = finished;
  s.current.transcribing = transcribing;

  // Reduced motion: no easing loop — just mirror the real value as it changes.
  const reduced = prefersReducedMotion();
  useEffect(() => {
    if (reduced) setDisplay(finished ? 1 : target);
  }, [reduced, finished, target]);

  useEffect(() => {
    if (reduced) return;
    const st = s.current;
    const tick = (ts: number) => {
      const dt = st.last ? Math.min(0.1, (ts - st.last) / 1000) : 0;
      st.last = ts;
      let d = st.display;
      if (st.finished) {
        d += (1 - d) * Math.min(1, dt * 8);
        if (d > 0.999) d = 1;
      } else if (st.transcribing) {
        // Creep toward a near-done ceiling; decays as it approaches so it never
        // quite arrives until the real "done" lands.
        const ceiling = Math.max(st.target, 0.96);
        d += (ceiling - d) * Math.min(1, dt * 0.14);
        if (st.target > d) d += (st.target - d) * Math.min(1, dt * 4);
        if (d > 0.985) d = 0.985;
      } else {
        // Early stages: just follow the real number, don't run ahead of it.
        d += (st.target - d) * Math.min(1, dt * 5);
      }
      if (Math.abs(d - st.display) > 0.0005 || d === 1) {
        st.display = d;
        setDisplay(d);
      } else {
        st.display = d;
      }
      st.raf = requestAnimationFrame(tick);
    };
    st.raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(st.raf);
    // The loop reads target/finished/transcribing from the ref, so it's set up
    // once; reduced-motion is decided at mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return display;
}

/**
 * Tick a number up like a mechanical counter. Used for word counts and the
 * progress percentage — mono, tabular, stepping rather than sliding.
 */
export function useCountUp(
  ref: React.RefObject<HTMLElement | null>,
  value: number,
  format: (n: number) => string = (n) => String(Math.round(n)),
) {
  const prev = useRef(0);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    if (prefersReducedMotion()) {
      el.textContent = format(value);
      prev.current = value;
      return;
    }

    const state = { n: prev.current };
    const tween = gsap.to(state, {
      n: value,
      duration: Math.min(1.1, 0.3 + Math.abs(value - prev.current) / 4000),
      ease: EASE.snap,
      onUpdate: () => {
        el.textContent = format(state.n);
      },
      onComplete: () => {
        prev.current = value;
      },
    });

    return () => {
      tween.kill();
    };
  }, [value, ref, format]);
}
