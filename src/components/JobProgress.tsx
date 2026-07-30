import gsap from "gsap";

import type { JobState } from "../lib/api";
import { fileName } from "../lib/format";
import { EASE, useGsap, useSmoothProgress } from "../lib/motion";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

type Props = {
  job: JobState;
  onDismiss: () => void;
};

// A segmented bar rather than a continuous fill — §5 "flush segments", and it
// gives the progress a mechanical tick instead of a smooth crawl.
const SEGMENTS = 32;

export function JobProgress({ job, onDismiss }: Props) {
  const failed = job.status === "error";
  // Whisper reports nothing while a chunk transcribes, so smooth the raw value
  // into a bar that keeps moving. `stage` tells us when we're in that phase.
  const transcribing = /transcrib/i.test(job.stage);
  const display = useSmoothProgress(job.progress, job.status === "done", transcribing);
  const pct = Math.round(display * 100);
  const filled = Math.round(display * SEGMENTS);

  const scope = useGsap(({ scope }) => {
    gsap.fromTo(
      scope.querySelectorAll("[data-line]"),
      { opacity: 0, y: 8 },
      { opacity: 1, y: 0, duration: 0.35, ease: EASE.snap, stagger: 0.05 },
    );
  }, [failed]);

  // Newly-filled segments snap on rather than fading.
  useGsap(({ scope }) => {
    gsap.fromTo(
      scope.querySelectorAll("[data-seg-on]"),
      { scaleY: 0.3, opacity: 0.4 },
      {
        scaleY: 1,
        opacity: 1,
        transformOrigin: "bottom",
        duration: 0.22,
        ease: EASE.step,
        overwrite: true,
      },
    );
  }, [filled]);

  return (
    <div ref={scope} className="dot-grid titlebar-pad drag-region flex h-full flex-col items-center justify-center px-10">
      <div className="no-drag w-full max-w-[460px] border border-hairline bg-panel">
        <div className="flex items-center justify-between border-b border-hairline px-4 py-2">
          <span className="micro flex items-center gap-2 text-faint">
            <span className={failed ? "text-amber" : "text-grey"}>
              <PixelCluster
                pattern={failed ? CLUSTERS.warn : CLUSTERS.bullet}
                size={2.5}
                pulse={!failed}
              />
            </span>
            {failed ? "FAILED" : "TRANSCRIBING"}
          </span>
          <span className="micro mono-data text-faint">WHISPER // MEDIUM</span>
        </div>

        <div className="px-4 py-4">
          <p
            data-line
            className="gsap-init selectable truncate font-mono text-[11px] text-ink"
          >
            {fileName(job.path)}
          </p>

          {failed ? (
            <>
              <p
                data-line
                className="gsap-init selectable mt-3 whitespace-pre-wrap font-mono text-[10px] leading-relaxed text-grey"
              >
                {job.error}
              </p>
              <button
                data-line
                onClick={onDismiss}
                className="gsap-init micro mt-4 border border-ink px-3 py-1.5 text-ink transition-colors hover:bg-ink hover:text-surface"
              >
                DISMISS
              </button>
            </>
          ) : (
            <>
              <div data-line className="gsap-init mt-4 flex gap-px">
                {Array.from({ length: SEGMENTS }, (_, i) => {
                  const on = i < filled;
                  return (
                    <span
                      key={i}
                      {...(on ? { "data-seg-on": "" } : {})}
                      className={[
                        "h-4 flex-1",
                        on ? "bg-ink" : "bg-hairline-soft",
                      ].join(" ")}
                    />
                  );
                })}
              </div>

              <div
                data-line
                className="gsap-init mt-2.5 flex items-baseline justify-between"
              >
                <span className="micro text-grey">{job.stage}</span>
                <span className="mono-data text-[13px] font-bold text-ink">
                  {pct}%
                </span>
              </div>
            </>
          )}
        </div>

        <p className="diagnostic border-t border-hairline px-4 py-2">
          {failed ? "NO DATA WRITTEN" : "LOCAL // NOTHING UPLOADED"}
        </p>
      </div>
    </div>
  );
}
