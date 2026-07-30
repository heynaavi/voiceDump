import { useEffect, useRef } from "react";
import gsap from "gsap";

type Props = {
  /** Rects the swarm must not land on — §4.1 "mask under data AND headline". */
  exclude?: React.RefObject<HTMLElement | null>[];
  /** Nudges overall density; the field should stay atmosphere, never content. */
  intensity?: number;
  className?: string;
};

// §4.1: walk a grid and draw a square with probability p. The grid walk is what
// makes the marks read as *pixels* rather than noise.
const PITCH = 12; // screen px (print spec is ~6.6pt on a 960pt page)
const SQUARE = 5; // ~half the pitch
const BASE_P = 0.045;

// §B3 cover spec: "per-square opacity ≈ 0.04–0.16 (very quiet)". The §4.1
// range tops out at 0.26, which is far too loud full-bleed behind live UI.
const ALPHA_MIN = 0.04;
const ALPHA_MAX = 0.16;

type Blob = { x: number; y: number; sx: number; sy: number; amp: number };

/**
 * The signature motif: grid-aligned square pixels scattered like a swarm — an
 * even sparse haze overall, thickening into one or two soft clusters.
 *
 * Drawn to canvas rather than DOM because a full-bleed field is a few thousand
 * squares; that many divs would cost real layout time on every resize.
 */
export function SwarmField({ exclude = [], intensity = 1, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    let blobs: Blob[] = [];

    const layout = () => {
      const parent = canvas.parentElement;
      if (!parent) return { w: 0, h: 0 };
      const { width: w, height: h } = parent.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.max(1, Math.floor(w * dpr));
      canvas.height = Math.max(1, Math.floor(h * dpr));
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

      // One dominant cluster plus a few weaker ones — reads as a swarm that
      // leans to one side yet still speckles everywhere.
      blobs = [
        { x: w * 0.8, y: h * 0.26, sx: w * 0.28, sy: h * 0.32, amp: 0.26 },
        { x: w * 0.18, y: h * 0.74, sx: w * 0.24, sy: h * 0.28, amp: 0.12 },
        { x: w * 0.54, y: h * 0.1, sx: w * 0.2, sy: h * 0.18, amp: 0.07 },
      ];
      return { w, h };
    };

    // Exclusion rects in canvas-local space, padded per §4.1 (~8-10pt).
    const exclusions = (): DOMRect[] => {
      const host = canvas.parentElement?.getBoundingClientRect();
      if (!host) return [];
      const pad = 14;
      return exclude
        .map((r) => r.current?.getBoundingClientRect())
        .filter((r): r is DOMRect => !!r)
        .map(
          (r) =>
            new DOMRect(
              r.left - host.left - pad,
              r.top - host.top - pad,
              r.width + pad * 2,
              r.height + pad * 2,
            ),
        );
    };

    const draw = () => {
      const { w, h } = layout();
      if (!w || !h) return;

      const zones = exclusions();
      ctx.clearRect(0, 0, w, h);

      // Deterministic per-cell pseudo-random, so a resize redraw reproduces
      // the identical field instead of reshuffling it.
      const hash = (x: number, y: number) => {
        const n = Math.sin(x * 127.1 + y * 311.7) * 43758.5453;
        return n - Math.floor(n);
      };

      for (let gx = 0; gx < w + PITCH; gx += PITCH) {
        for (let gy = 0; gy < h + PITCH; gy += PITCH) {
          let p = BASE_P;
          for (const b of blobs) {
            const dx = (gx - b.x) / b.sx;
            const dy = (gy - b.y) / b.sy;
            p += b.amp * Math.exp(-(dx * dx + dy * dy));
          }
          const r = hash(gx, gy);
          if (r > p) continue;

          if (zones.some((z) => gx >= z.x && gx <= z.x + z.width && gy >= z.y && gy <= z.y + z.height)) {
            continue;
          }

          // §4.1: low, *varied* per-square opacity gives depth without volume.
          const alpha = ALPHA_MIN + hash(gx + 1, gy) * (ALPHA_MAX - ALPHA_MIN);

          ctx.fillStyle = `rgba(143, 176, 124, ${alpha})`;
          ctx.fillRect(Math.round(gx), Math.round(gy), SQUARE, SQUARE);
        }
      }
    };

    draw();

    // Redrawing thousands of cells is far too expensive to do per frame — an
    // earlier version tweened the field at 60fps and starved every other
    // animation on the page. The field is drawn once and only redrawn on
    // resize; the "breathing" is a compositor-only opacity tween on the canvas
    // element itself, which costs nothing.
    let resizeTimer: number | undefined;
    const observer = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(draw, 120);
    });
    if (canvas.parentElement) observer.observe(canvas.parentElement);

    let tween: gsap.core.Tween | null = null;
    if (!reduced) {
      tween = gsap.fromTo(
        canvas,
        { opacity: 0.75 },
        {
          opacity: 1,
          duration: 7,
          ease: "sine.inOut",
          repeat: -1,
          yoyo: true,
        },
      );
    }

    return () => {
      window.clearTimeout(resizeTimer);
      observer.disconnect();
      tween?.kill();
    };
  }, [exclude]);

  return (
    // The wrapper carries `intensity` so it can change (e.g. on drag-over)
    // without touching the canvas opacity GSAP is tweening.
    <div
      aria-hidden
      className={`pointer-events-none absolute inset-0 transition-opacity duration-300 ${className ?? ""}`}
      style={{ opacity: intensity }}
    >
      <canvas ref={canvasRef} className="absolute inset-0" />
    </div>
  );
}
