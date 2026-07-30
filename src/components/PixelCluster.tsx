import { useEffect, useRef } from "react";
import gsap from "gsap";

type Props = {
  /** Which cells are lit. §4.4: 2x2 or 3x3 with 1-2 knocked out, like a QR fragment. */
  pattern?: boolean[];
  size?: number;
  gap?: number;
  className?: string;
  /** Assemble the cells one at a time on mount. */
  animate?: boolean;
  /** Loop a slow cell-by-cell reshuffle — used for the working/pending state. */
  pulse?: boolean;
};

// §4.4 replaces all stroke icons. A few stock fragments so different call sites
// don't all show the identical mark.
export const CLUSTERS = {
  brand: [true, true, false, true, true, true, false, true, true],
  file: [true, true, true, true, false, true, true, true, false],
  bullet: [true, false, true, true],
  search: [false, true, true, true, true, false, true, false, true],
  done: [true, false, false, true, true, false, false, true, true],
  warn: [false, true, false, true, true, true, true, false, true],
} satisfies Record<string, boolean[]>;

export function PixelCluster({
  pattern = CLUSTERS.brand,
  size = 3,
  gap = 1.5,
  className,
  animate = false,
  pulse = false,
}: Props) {
  const ref = useRef<HTMLSpanElement>(null);
  const cols = Math.sqrt(pattern.length) === 2 ? 2 : 3;
  const extent = cols * size + (cols - 1) * gap;

  useEffect(() => {
    const host = ref.current;
    if (!host) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

    const cells = host.querySelectorAll("i");
    const timelines: gsap.core.Timeline[] = [];

    if (animate) {
      // Snap in cell by cell — stepped, not eased, so it reads mechanical.
      const tl = gsap.timeline();
      tl.fromTo(
        cells,
        { opacity: 0, scale: 0.2 },
        {
          opacity: 1,
          scale: 1,
          duration: 0.16,
          ease: "steps(3)",
          stagger: { each: 0.035, from: "random" },
        },
      );
      timelines.push(tl);
    }

    if (pulse) {
      // Cells blink out and back in a rolling pattern — a "thinking" indicator
      // that stays in the pixel language instead of borrowing a spinner.
      const tl = gsap.timeline({ repeat: -1 });
      tl.to(cells, {
        opacity: 0.25,
        duration: 0.3,
        ease: "steps(2)",
        stagger: { each: 0.08, from: "start", yoyo: true, repeat: 1 },
      });
      timelines.push(tl);
    }

    return () => timelines.forEach((t) => t.kill());
  }, [animate, pulse]);

  return (
    <span
      ref={ref}
      aria-hidden
      className={`relative inline-block shrink-0 ${className ?? ""}`}
      style={{ width: extent, height: extent }}
    >
      {pattern.map((on, i) =>
        on ? (
          <i
            key={i}
            className="absolute bg-current"
            style={{
              width: size,
              height: size,
              left: (i % cols) * (size + gap),
              top: Math.floor(i / cols) * (size + gap),
            }}
          />
        ) : null,
      )}
    </span>
  );
}
