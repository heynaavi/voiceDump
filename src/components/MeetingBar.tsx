import { useEffect, useRef, useState } from "react";

import { watchMeetingLevels } from "../lib/api";
import type { Detected, MeetingProgress } from "../lib/api";
import { formatTimestamp } from "../lib/format";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

type Props = {
  /** `null` when no meeting is in flight — the bar renders nothing. */
  phase: "recording" | "finishing" | null;
  /** `Date.now()` at the moment recording started, for the elapsed clock. */
  startedAt: number | null;
  /**
   * How far the two transcriptions have got, or null before the first report.
   *
   * Passed in rather than subscribed to here, because the event that carries it
   * is also the event that decides the phase: a meeting stopped from the
   * floating card is announced to this window only as progress, and something
   * that only listened while `phase === "finishing"` would never hear the
   * report that was supposed to set it.
   */
  progress: MeetingProgress | null;
  onStop: () => void;
  /** An app has the microphone open and we have not been told to shut up. */
  detected: Detected | null;
  onTakeNotes: () => void;
};

/** Cells per meter. Matches the recorder's meter so the two read as one app. */
const CELLS = 14;

/**
 * A meter that decays rather than snapping to zero.
 *
 * Levels arrive every 50 ms, and speech is full of gaps that short — so a meter
 * that follows the number exactly spends most of a sentence flickering to
 * nothing and reads as a broken connection. Falling back gradually shows what a
 * person means by "this side is talking".
 */
function useDecayingLevel(side: "you" | "others") {
  const [level, setLevel] = useState(0);
  const target = useRef(0);

  useEffect(() => {
    const sub = watchMeetingLevels((l) => {
      if (l.side === side) target.current = l.level;
    });

    let frame = 0;
    const tick = () => {
      setLevel((current) => {
        const next = target.current;
        // Rise immediately, fall slowly: the ear hears an onset as instant and
        // a tail as gradual, and the eye should agree with it.
        return next > current ? next : current + (next - current) * 0.18;
      });
      // Decay toward silence between packets, so a side that stops sending
      // levels entirely still settles instead of freezing mid-bar.
      target.current *= 0.94;
      frame = requestAnimationFrame(tick);
    };
    tick();

    return () => {
      cancelAnimationFrame(frame);
      sub.then((un) => un()).catch(() => {});
    };
  }, [side]);

  return level;
}

function Meter({ label, level }: { label: string; level: number }) {
  return (
    <div className="flex items-center gap-2">
      <span className="micro w-[46px] shrink-0 text-faint">{label}</span>
      <div className="flex items-center gap-[2px]">
        {Array.from({ length: CELLS }, (_, i) => (
          <span
            key={i}
            className={[
              "block w-[3px] transition-[height,background-color] duration-75",
              i / CELLS < level ? "bg-sage-dim" : "bg-hairline",
            ].join(" ")}
            style={{ height: 4 + (i % 3) * 3 + (i / CELLS < level ? 7 : 0) }}
          />
        ))}
      </div>
    </div>
  );
}

/**
 * The meeting readout: live while recording, then the two transcriptions.
 *
 * Rendered over the pane rather than inside the empty state, because a call
 * outlives whatever the user was reading when it started — moving to another
 * note must not look like the recording stopped.
 *
 * Where it floats is App's business, not this file's: the cards below lay out
 * in flow and the corner they sit in is decided once, in one place, next to
 * everything else that floats. They used to place themselves, which is how
 * three of them ended up claiming the bottom centre.
 */
export function MeetingBar({
  phase,
  startedAt,
  progress,
  onStop,
  detected,
  onTakeNotes,
}: Props) {
  const [elapsed, setElapsed] = useState(0);
  const you = useDecayingLevel("you");
  const others = useDecayingLevel("others");

  useEffect(() => {
    if (phase !== "recording" || startedAt === null) return;
    const tick = () => setElapsed((Date.now() - startedAt) / 1000);
    tick();
    const timer = setInterval(tick, 250);
    return () => clearInterval(timer);
  }, [phase, startedAt]);

  // Nothing recording, but something is on the microphone: offer, once.
  if (phase === null && detected) {
    return (
      <div className="w-[420px] max-w-full">
        <div className="pointer-events-auto flex items-center gap-3 border border-ink bg-panel px-3 py-2.5 shadow-[0_8px_24px_rgba(0,0,0,0.2)]">
          <span className="text-sage-dim">
            <PixelCluster pattern={CLUSTERS.brand} size={5} gap={2} pulse />
          </span>
          <div className="min-w-0 flex-1">
            <p className="micro text-ink">MEETING DETECTED</p>
            <p className="mt-0.5 truncate text-[11px] text-faint">
              {detected.name} is using your microphone
            </p>
          </div>
          {/* No decline button. The offer expires on its own after six
              seconds — here and on the floating card, which share a clock — so
              a button asking someone to decide is asking them to do work that
              doing nothing already does. */}
          <button
            onClick={onTakeNotes}
            className="micro shrink-0 border border-ink bg-ink px-3 py-2 text-surface transition-colors hover:bg-transparent hover:text-ink"
          >
            TAKE NOTES
          </button>
        </div>
      </div>
    );
  }

  if (phase === null) return null;

  if (phase === "finishing") {
    const fraction = progress?.progress ?? 0;
    return (
      <div className="w-[420px] max-w-full">
        <div className="border border-ink bg-panel shadow-[0_8px_24px_rgba(0,0,0,0.2)]">
          <div className="flex items-center justify-between border-b border-hairline-soft px-3 py-1.5">
            <span className="micro text-ink">FINISHING MEETING</span>
            <span className="mono-data text-[11px] tabular-nums text-faint">
              {Math.round(fraction * 100)}%
            </span>
          </div>
          <div className="px-3 py-2.5">
            <p className="micro text-grey">
              {(progress?.stage ?? "Stopping").toUpperCase()}
            </p>
            {/* Segmented, like every other progress bar in the app. */}
            <div className="mt-2 flex gap-px">
              {Array.from({ length: 32 }, (_, i) => (
                <span
                  key={i}
                  className={[
                    "h-1.5 flex-1",
                    i / 32 < fraction ? "bg-sage-dim" : "bg-hairline",
                  ].join(" ")}
                />
              ))}
            </div>
            <p className="micro mt-2 text-faint">
              BOTH SIDES ARE TRANSCRIBED SEPARATELY
            </p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div>
      <div className="pointer-events-auto flex items-center gap-3 border border-ink bg-panel px-3 py-2 shadow-[0_8px_24px_rgba(0,0,0,0.2)]">
        <button
          onClick={onStop}
          aria-label="Stop recording the meeting"
          className="flex h-9 w-9 shrink-0 items-center justify-center border border-amber bg-amber text-surface transition-colors hover:bg-transparent hover:text-amber"
        >
          <span className="block h-3 w-3 bg-current" />
        </button>

        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <span className="micro text-ink">MEETING</span>
            <span className="mono-data text-[11px] tabular-nums text-faint">
              {formatTimestamp(elapsed)}
            </span>
          </div>
          <Meter label="YOU" level={you} />
          <Meter label="OTHERS" level={others} />
        </div>
      </div>
    </div>
  );
}
