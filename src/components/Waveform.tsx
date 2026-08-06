import { useEffect, useRef } from "react";

type Props = {
  /** Normalised 0..1 amplitude buckets from the sidecar. */
  peaks: number[];
  /** Playback position as a fraction of the total, 0..1. */
  progress: number;
  /** Hovered position, 0..1, or null when the pointer is elsewhere. */
  hover: number | null;
};

const BAR_W = 2;
const GAP = 1;

/**
 * The player's scrub surface, drawn as vertical bars.
 *
 * Canvas rather than 900 DOM nodes: this repaints on every animation frame
 * during playback, and a bar-per-element would spend the frame budget on style
 * recalculation. Redraws are cheap — one path per colour band.
 */
export function Waveform({ peaks, progress, hover }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // Read in the draw effect without making it a dependency, so moving the
  // pointer doesn't tear down and rebuild the resize observer.
  const stateRef = useRef({ progress, hover });
  stateRef.current = { progress, hover };

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const parent = canvas.parentElement;
    if (!parent) return;

    let frame = 0;

    const draw = () => {
      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      const dpr = window.devicePixelRatio || 1;
      const w = parent.clientWidth;
      const h = parent.clientHeight;
      if (!w || !h) return;

      if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
        canvas.width = w * dpr;
        canvas.height = h * dpr;
        canvas.style.width = `${w}px`;
        canvas.style.height = `${h}px`;
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);

      const slots = Math.max(1, Math.floor(w / (BAR_W + GAP)));
      const styles = getComputedStyle(canvas);
      const played = styles.getPropertyValue("--wave-played").trim() || "#1f2a24";
      const ahead = styles.getPropertyValue("--wave-ahead").trim() || "#c8d5cb";

      const { progress: p, hover: hv } = stateRef.current;
      const playedSlots = Math.round(p * slots);
      const hoverSlot = hv === null ? -1 : Math.round(hv * slots);

      for (let i = 0; i < slots; i++) {
        // Resample the fixed bucket count onto however many bars fit. Taking
        // the max of the span keeps transients visible when downsampling.
        const from = Math.floor((i / slots) * peaks.length);
        const to = Math.max(from + 1, Math.floor(((i + 1) / slots) * peaks.length));
        let v = 0;
        for (let j = from; j < to && j < peaks.length; j++) {
          if (peaks[j] > v) v = peaks[j];
        }

        // A floor keeps silence as a visible baseline rather than a gap, so the
        // strip still reads as a continuous, clickable timeline.
        const barH = Math.max(2, v * (h - 4));
        const x = i * (BAR_W + GAP);
        const y = (h - barH) / 2;

        ctx.fillStyle = i < playedSlots ? played : ahead;
        ctx.globalAlpha = hoverSlot >= 0 && i <= hoverSlot && i >= playedSlots ? 0.55 : 1;
        ctx.fillRect(x, y, BAR_W, barH);
      }
      ctx.globalAlpha = 1;
    };

    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(draw);
    };

    schedule();
    const ro = new ResizeObserver(schedule);
    ro.observe(parent);
    return () => {
      cancelAnimationFrame(frame);
      ro.disconnect();
    };
  }, [peaks, progress, hover]);

  // Absolutely positioned on purpose: the canvas measures its parent, so if it
  // also contributed to the parent's height each resize would feed back into
  // the next measurement and the player would grow without bound.
  return (
    <canvas ref={canvasRef} className="wave-canvas absolute inset-0 h-full w-full" />
  );
}
