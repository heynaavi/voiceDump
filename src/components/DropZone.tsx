import { useMemo, useRef } from "react";
import gsap from "gsap";

import type { MeetingCapability } from "../lib/api";
import { EASE, useGsap } from "../lib/motion";
import { CLUSTERS, PixelCluster } from "./PixelCluster";
import { Recorder } from "./Recorder";
import { SwarmField } from "./SwarmField";

type Props = {
  dragging: boolean;
  onBrowse: () => void;
  onRecorded: (path: string) => void;
  assistantError: string | null;
  /** Null until the backend has been asked whether this Mac can capture a call. */
  meeting: MeetingCapability | null;
  onStartMeeting: () => void;
  /** Set when the last attempt to start a meeting was refused. */
  meetingError: string | null;
};

export function DropZone({
  dragging,
  onBrowse,
  onRecorded,
  assistantError,
  meeting,
  onStartMeeting,
  meetingError,
}: Props) {
  const titleRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // Stable identity: a fresh array here would re-run the field's effect on
  // every render, rebuilding the observer and redrawing thousands of cells.
  const excludeZones = useMemo(() => [titleRef, panelRef], []);

  const scope = useGsap(({ scope }) => {
    const tl = gsap.timeline();
    tl.fromTo(
      scope.querySelectorAll("[data-line]"),
      { opacity: 0, y: 10 },
      { opacity: 1, y: 0, duration: 0.4, ease: EASE.snap, stagger: 0.06 },
    ).fromTo(
      scope.querySelectorAll("[data-tile]"),
      { opacity: 0, scaleY: 0.7 },
      {
        opacity: 1,
        scaleY: 1,
        transformOrigin: "top",
        duration: 0.3,
        ease: EASE.step,
        stagger: 0.05,
      },
      "-=0.15",
    );
  }, []);

  return (
    <div ref={scope} className="dot-grid relative h-full overflow-hidden">
      {/* §4.1 full-bleed but quiet, masked out from under the headline and the
          data container. */}
      <SwarmField exclude={excludeZones} intensity={dragging ? 1 : 0.72} />

      <div className="titlebar-pad drag-region relative flex h-full flex-col items-center justify-center px-10">
        <div ref={titleRef} className="w-full max-w-[440px]">
          <p data-line className="gsap-init eyebrow text-faint">
            00_INGEST // DROP TO BEGIN
          </p>
          <h1
            data-line
            className="gsap-init mt-2 text-[22px] font-medium leading-tight tracking-[-0.01em] text-ink"
          >
            {dragging ? "Release to transcribe" : "Drop audio or video"}
          </h1>
        </div>

        {/* Rung 3 — outline frame. It's a target, not the page's subject. */}
        <div
          ref={panelRef}
          className={[
            "no-drag relative mt-5 w-full max-w-[440px] border bg-panel transition-colors duration-150",
            dragging ? "border-sage-dim" : "border-hairline",
          ].join(" ")}
        >
          <div className="flex items-center justify-between border-b border-hairline-soft px-4 py-2">
            <span className="micro text-faint">SUPPORTED</span>
            <span className="micro mono-data text-faint">28 FORMATS</span>
          </div>

          <div className="grid grid-cols-4 gap-px bg-hairline-soft">
            {["MP3", "M4A", "WAV", "FLAC", "MP4", "MOV", "MKV", "WEBM"].map((f) => (
              <div
                key={f}
                data-tile
                className="gsap-init micro bg-panel px-2 py-2 text-center text-grey"
              >
                {f}
              </div>
            ))}
          </div>

          <div className="flex items-center justify-between border-t border-hairline-soft px-4 py-3">
            <span className="flex items-center gap-2 text-faint">
              <PixelCluster pattern={CLUSTERS.bullet} size={2.5} />
              <span className="micro">RUNS ON THIS MAC</span>
            </span>
            <button
              onClick={onBrowse}
              className="micro border border-ink bg-ink px-3 py-1.5 text-surface transition-colors hover:bg-transparent hover:text-ink"
            >
              CHOOSE FILE
            </button>
          </div>
        </div>

        {/* The other way in: no file yet, just talk. Deliberately below the
            drop target — this is the secondary path. */}
        <div data-line className="gsap-init no-drag mt-6 flex flex-col items-center">
          <div className="mb-4 flex w-full max-w-[440px] items-center gap-3">
            <span className="h-px flex-1 bg-hairline" />
            <span className="micro text-faint">OR</span>
            <span className="h-px flex-1 bg-hairline" />
          </div>
          <Recorder onCaptured={onRecorded} />

          {/* The third way in, and the only one that hears someone else. Kept
              on the same rung as the microphone button: both are "start
              talking", and the difference is who else is in the room. */}
          {meeting?.available && (
            <button
              onClick={onStartMeeting}
              className="micro mt-2.5 flex items-center gap-2.5 border border-hairline px-4 py-2.5 text-grey transition-colors hover:border-ink hover:bg-ink hover:text-surface"
            >
              <span className="block h-2.5 w-2.5 bg-sage-dim" />
              RECORD A MEETING
            </button>
          )}

          {/* An unavailable feature explains itself once, quietly, rather than
              showing a button that will only ever fail. */}
          {meeting && !meeting.available && (
            <p className="micro mt-2.5 max-w-[320px] text-center leading-relaxed text-faint">
              {meeting.reason}
            </p>
          )}

          {meetingError && (
            <p className="micro mt-2.5 max-w-[360px] text-center leading-relaxed text-amber">
              {meetingError}
            </p>
          )}
        </div>

        {assistantError && (
          <div className="no-drag mt-5 w-full max-w-[440px] border border-amber bg-panel">
            <p className="micro border-b border-hairline-soft bg-amber px-3 py-1.5 text-surface">
              AI LAYER OFFLINE
            </p>
            <p className="selectable whitespace-pre-wrap px-3 py-2.5 font-mono text-[10px] leading-relaxed text-grey">
              {assistantError}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
