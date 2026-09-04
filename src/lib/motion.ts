import { useEffect, useLayoutEffect, useRef, useState } from "react";
import gsap from "gsap";

/**
 * Motion vocabulary — QWEE Design System V3 §7, on the app surface.
 *
 * V3 replaced V1's motion chapter outright, and the app had been running on
 * V1's: one curve (`power4.out`) for everything, a stepped ease left over from
 * a register the app is not in, and nine entrances with nine different sets of
 * numbers because each was tuned where it was written.
 *
 * Two parts of V3 govern what is here. §7.2 gives a curve per *gesture*, which
 * is the part the app was missing — a panel arriving and a strike bar landing
 * are not the same event and should not share an ease. §9 gives the tempo for
 * this surface: **12–20 frames**, which is 0.20–0.33s. The 720 ms reveal in
 * §7.3 is the website's; a tab that took 720 ms to change would be the lag this
 * app has already been asked twice to get rid of.
 *
 * The stepped easing goes. V3 puts 2-frame snaps and no fades in the PRESS
 * register (§9), and this surface is Field Notes: "in place, eased".
 */
export const EASE = {
  /** A panel, a section, a list arriving. §7.2 "scroll-in reveals". */
  reveal: "expo.out",
  /** Decisive arrivals: type, wipes, strike bars. */
  snap: "power4.out",
  /** Layout reflow — unremarkable on purpose. */
  reflow: "power2.inOut",
  /** Leaving: a preview dissolving, a card folding. */
  leave: "power2.in",
  /** Depth pushes, group moves. */
  push: "power3.inOut",
  /** A word fading in where it stays. */
  word: "power1.out",
  /** Drift, breath — should not appear commanded. */
  drift: "sine.inOut",
  /** Something arriving under its own weight — the pill, a badge. */
  arrive: "back.out(1.7)",
  /** Clocks, counters, typing progress. Nothing to interpret. */
  none: "none",
} as const;

/**
 * The measured numbers, in one place — V3 §7.3 and §9.
 *
 * They were nine sets of numbers in nine files. Nobody chose the difference
 * between a 0.34 s entrance and a 0.42 s one; they were written months apart.
 */
export const BEAT = {
  /** 18 frames. Inside §9's 12–20 for this surface, and the app's own median. */
  reveal: 0.3,
  /** 12 frames. The floor, for something small resolving in place. */
  quick: 0.2,
  /**
   * 20 frames — the top of §9's band for this surface, and the only gesture
   * that earns it: the reading view resizing every paragraph at once while
   * holding the reader's place. A reflow is the one thing that should be
   * unremarkable rather than quick.
   */
  reflow: 0.33,
  /** §7.3: 70 ms between siblings. */
  stagger: 0.07,
  /** §7.3: "cap 8" — a group's whole entrance is bounded, however long it is. */
  siblings: 8,
  /**
   * How far a reveal rises.
   *
   * §7.3's figure is 18px, and that is a web number: a section coming up into a
   * scrolling page. §1 is the rule that governs here — **nothing travels** —
   * and at 13px body text an 18px rise is travel. Ten is a resolve.
   */
  rise: 10,
} as const;

/**
 * The stagger for a group of siblings, bounded however many there are.
 *
 * Under the cap it is 70 ms each, which is the measured figure. Over it the
 * whole group shares the time eight would have taken, so a list of twelve and a
 * list of eight hundred both finish in the same half-second. That is what §7.3's
 * "cap 8" has to mean for a group whose size is data rather than layout.
 */
export function spread(count: number): gsap.StaggerVars {
  const whole = BEAT.stagger * BEAT.siblings;
  return count > BEAT.siblings ? { amount: whole } : { each: BEAT.stagger };
}

/**
 * The app's one entrance: a group resolving into place where it already is.
 *
 * Every panel, sheet and list in the app used to write this out by hand, and no
 * two agreed — rises of 8, 10 and 14, durations from 0.28 to 0.42, staggers
 * from 0.045 to 0.07, all on the curve V3 reserves for decisive arrivals. One
 * function is the point: the gesture is a property of the design system, not of
 * whichever file it happens in.
 */
export function reveal(
  targets: gsap.TweenTarget,
  vars: gsap.TweenVars = {},
): gsap.core.Tween {
  const count = gsap.utils.toArray(targets).length;
  return gsap.fromTo(
    targets,
    { opacity: 0, y: BEAT.rise },
    {
      opacity: 1,
      y: 0,
      duration: BEAT.reveal,
      ease: EASE.reveal,
      stagger: spread(count),
      ...vars,
    },
  );
}

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
 *
 * Two things here come straight from V3 §7. The curve is `none`: §7.2 gives
 * that to "clocks, counters, typing progress", and a counter that decelerates
 * is making a claim about the data rather than about the animation. And it
 * steps no faster than [`COUNTER_STEP`] — §7.3, "counter steps ≥ 8 frames
 * apart; under that a number reads as flicker, not as changed". It used to
 * write a new number on every frame, which for a word count going to four
 * digits is sixty unreadable numbers a second.
 */
/**
 * The shortest time a shown number is allowed to stand for.
 *
 * V3 §7.3: "counter steps ≥ 8 frames apart — under that a number reads as
 * flicker, not as changed". Eight frames at 60 Hz, in seconds, because that is
 * what the tween's own clock is in — and in real time rather than as a fraction
 * of the tween, whose length varies from 0.3s to 1.1s with how far the number
 * has to travel.
 */
const COUNTER_STEP = 8 / 60;

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
    let shown = -1;
    const tween = gsap.to(state, {
      n: value,
      duration: Math.min(1.1, 0.3 + Math.abs(value - prev.current) / 4000),
      ease: EASE.none,
      onUpdate: () => {
        // Held back to the step rate, and by `progress` rather than a clock:
        // GSAP's ticker is the only thing driving this, so counting its own
        // frames is what "eight frames apart" actually means here.
        const step = Math.floor(tween.time() / COUNTER_STEP);
        if (step === shown) return;
        shown = step;
        el.textContent = format(state.n);
      },
      onComplete: () => {
        // The last number is the real one, whatever the step rate had reached.
        el.textContent = format(value);
        prev.current = value;
      },
    });

    return () => {
      tween.kill();
    };
  }, [value, ref, format]);
}
