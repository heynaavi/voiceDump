import { useCallback, useEffect, useState } from "react";
import gsap from "gsap";
import { listen } from "@tauri-apps/api/event";

import { modelsFetch, type ModelProgress, type ModelStatus } from "../lib/api";
import { EASE, useGsap } from "../lib/motion";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

type Props = {
  /** What the backend says is missing. Only rendered when `ready` is false. */
  status: ModelStatus;
  /** Everything is on disk — hand the window back to the app. */
  onReady: () => void;
};

// The same segmented bar the transcription progress uses, so a download reads
// as this app working rather than as an installer bolted onto the front of it.
const SEGMENTS = 32;

function megabytes(bytes: number): string {
  const mb = bytes / 1_000_000;
  return mb >= 1000 ? `${(mb / 1000).toFixed(2)} GB` : `${Math.round(mb)} MB`;
}

/**
 * First run: fetch the speech models.
 *
 * This screen exists because the models stopped shipping inside the app — see
 * `src-tauri/src/models.rs` for why. It is a gate rather than a banner: there
 * is no useful version of this app without weights, so offering it in the
 * background would only mean a dictation key that silently does nothing.
 *
 * It is shown once. The files land beside the database, so upgrading the app
 * later finds them already there and this never appears again.
 */
export function ModelSetup({ status, onReady }: Props) {
  const [progress, setProgress] = useState<ModelProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    const sub = listen<ModelProgress>("model-progress", (e) =>
      setProgress(e.payload),
    );
    return () => {
      sub.then((off) => off());
    };
  }, []);

  const start = useCallback(() => {
    setError(null);
    setRunning(true);
    modelsFetch()
      .then(onReady)
      .catch((e) => {
        setError(String(e));
        setRunning(false);
        setProgress(null);
      });
  }, [onReady]);

  const done = progress ? progress.received / Math.max(1, progress.total) : 0;
  const filled = Math.round(done * SEGMENTS);
  // Verifying is a real pause on half a gigabyte — say so rather than letting a
  // full bar sit there looking stuck.
  const stage = progress?.verifying
    ? "VERIFYING"
    : running
      ? "DOWNLOADING"
      : "READY WHEN YOU ARE";

  const scope = useGsap(
    ({ scope }) => {
      gsap.fromTo(
        scope.querySelectorAll("[data-line]"),
        { opacity: 0, y: 8 },
        { opacity: 1, y: 0, duration: 0.38, ease: EASE.snap, stagger: 0.06 },
      );
    },
    [running, error !== null],
  );

  return (
    <div
      ref={scope}
      className="dot-grid titlebar-pad drag-region flex h-full flex-col items-center justify-center px-10"
    >
      <div className="no-drag w-full max-w-[460px] border border-hairline bg-panel">
        <div className="flex items-center justify-between border-b border-hairline px-4 py-2">
          <span className="micro flex items-center gap-2 text-faint">
            <span className={error ? "text-amber" : "text-sage-dim"}>
              <PixelCluster
                pattern={error ? CLUSTERS.warn : CLUSTERS.brand}
                size={2.5}
                pulse={running}
              />
            </span>
            {error ? "SETUP FAILED" : "FIRST RUN"}
          </span>
          <span className="micro mono-data text-faint">
            {progress ? `${progress.index} OF ${progress.count}` : "WHISPER"}
          </span>
        </div>

        <div className="px-4 py-4">
          <p data-line className="gsap-init text-[15px] text-ink">
            {error
              ? "That download did not finish"
              : running
                ? `Getting the ${progress?.label ?? "speech"} model`
                : "One download, then you are offline for good"}
          </p>

          {/* On a failure the pitch has already been read once, and repeating
              it above the error would bury the only line that matters. */}
          <p
            data-line
            className="gsap-init mt-2 text-[12px] leading-relaxed text-grey"
          >
            {error
              ? "Nothing was kept, so trying again is safe. Anything already downloaded picks up where it stopped."
              : running
                ? "Keep the app open. An interrupted download picks up where it stopped."
                : `The speech models are ${megabytes(status.bytes)} and are not part of the app, so updates stay small. They are saved next to your notes and stay put when you upgrade — this screen is a one-off.`}
          </p>

          {error ? (
            <p
              data-line
              className="gsap-init selectable mt-3 whitespace-pre-wrap text-[12px] leading-relaxed text-amber"
            >
              {error}
            </p>
          ) : (
            running && (
              <>
                <div data-line className="gsap-init mt-4 flex gap-px">
                  {Array.from({ length: SEGMENTS }, (_, i) => (
                    <span
                      key={i}
                      className={[
                        "h-4 flex-1",
                        i < filled ? "bg-ink" : "bg-hairline-soft",
                      ].join(" ")}
                    />
                  ))}
                </div>
                <div
                  data-line
                  className="gsap-init mt-2.5 flex items-baseline justify-between"
                >
                  <span className="micro text-grey">{stage}</span>
                  <span className="mono-data text-[13px] font-bold text-ink tabular-nums">
                    {progress
                      ? `${megabytes(progress.received)} / ${megabytes(progress.total)}`
                      : "—"}
                  </span>
                </div>
              </>
            )
          )}

          {!running && (
            <button
              data-line
              onClick={start}
              className="gsap-init micro mt-4 border border-ink px-3 py-1.5 text-ink transition-colors hover:bg-ink hover:text-surface"
            >
              {error ? "TRY AGAIN" : `DOWNLOAD ${megabytes(status.bytes)}`}
            </button>
          )}
        </div>

        <p className="diagnostic border-t border-hairline px-4 py-2">
          {error
            ? "NOTHING WAS KEPT // SAFE TO RETRY"
            : "FROM HUGGING FACE // CHECKSUMMED ON ARRIVAL"}
        </p>
      </div>
    </div>
  );
}
