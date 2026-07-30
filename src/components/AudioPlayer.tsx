import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";

import { formatTimestamp } from "../lib/format";
import { Waveform } from "./Waveform";

export type PlayerHandle = {
  /** Jump to a time. `resumeAfter` schedules an auto-resume once idle. */
  seek: (t: number, opts?: { pauseFirst?: boolean }) => void;
  /** Cancel a pending auto-resume — called when the user starts typing. */
  cancelResume: () => void;
};

type Props = {
  sourcePath: string;
  duration: number;
  peaks: number[] | null;
  /** Fired every animation frame while playing so the transcript can follow. */
  onTime: (seconds: number) => void;
  handleRef: React.MutableRefObject<PlayerHandle | null>;
};

const SPEEDS = [1, 1.25, 1.5, 2, 0.75];

/** How long a click-to-seek holds playback before resuming on its own. */
const RESUME_DELAY_MS = 4000;

/**
 * Floating transport pinned to the bottom of the reading column.
 *
 * Positioned `absolute` inside the content pane rather than `fixed` to the
 * window — fixed positioning centres on the whole window, which pushes the bar
 * visibly left of the text column once the sidebar takes its width.
 *
 * `onTime` is driven by a rAF loop rather than the `timeupdate` event: WebKit
 * fires that only ~4x/sec, which makes the word highlight lag the audio.
 */
export function AudioPlayer({
  sourcePath,
  duration,
  peaks,
  onTime,
  handleRef,
}: Props) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const [playing, setPlaying] = useState(false);
  const [current, setCurrent] = useState(0);
  const [speed, setSpeed] = useState(1);
  const [failed, setFailed] = useState(false);
  const [hover, setHover] = useState<number | null>(null);
  const [resumeIn, setResumeIn] = useState<number | null>(null);
  const scrubRef = useRef<HTMLDivElement>(null);
  const resumeTimer = useRef<number | null>(null);

  const total = duration || audioRef.current?.duration || 0;

  const cancelResume = useCallback(() => {
    if (resumeTimer.current !== null) {
      clearInterval(resumeTimer.current);
      resumeTimer.current = null;
    }
    setResumeIn(null);
  }, []);

  const seek = useCallback(
    (t: number, opts?: { pauseFirst?: boolean }) => {
      const audio = audioRef.current;
      if (!audio) return;

      audio.currentTime = Math.max(0, Math.min(t, total || t));
      setCurrent(audio.currentTime);
      onTime(audio.currentTime);

      if (!opts?.pauseFirst) return;

      // Clicking into the text is usually "let me look at this bit", so hold
      // playback long enough to read or start typing, then carry on by itself.
      // Typing cancels it via cancelResume().
      cancelResume();
      if (audio.paused) return;

      audio.pause();
      let left = RESUME_DELAY_MS;
      setResumeIn(left);
      resumeTimer.current = window.setInterval(() => {
        left -= 250;
        if (left <= 0) {
          cancelResume();
          audioRef.current?.play().catch(() => setFailed(true));
        } else {
          setResumeIn(left);
        }
      }, 250);
    },
    [onTime, total, cancelResume],
  );

  useEffect(() => {
    handleRef.current = { seek, cancelResume };
    return () => {
      handleRef.current = null;
    };
  }, [seek, cancelResume, handleRef]);

  useEffect(() => cancelResume, [cancelResume]);

  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    const tick = () => {
      const audio = audioRef.current;
      if (audio) {
        setCurrent(audio.currentTime);
        onTime(audio.currentTime);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, onTime]);

  const toggle = useCallback(() => {
    const audio = audioRef.current;
    if (!audio) return;
    cancelResume();
    if (audio.paused) audio.play().catch(() => setFailed(true));
    else audio.pause();
  }, [cancelResume]);

  // Space toggles playback unless the user is typing or editing prose.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (
        el &&
        (["INPUT", "TEXTAREA"].includes(el.tagName) || el.isContentEditable)
      ) {
        return;
      }
      if (e.code === "Space") {
        e.preventDefault();
        toggle();
      } else if (e.code === "ArrowLeft") {
        seek((audioRef.current?.currentTime ?? 0) - 5);
      } else if (e.code === "ArrowRight") {
        seek((audioRef.current?.currentTime ?? 0) + 5);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [seek, toggle]);

  const fractionAt = (clientX: number): number | null => {
    const bar = scrubRef.current;
    if (!bar) return null;
    const rect = bar.getBoundingClientRect();
    return Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  };

  const scrubTo = (clientX: number) => {
    const f = fractionAt(clientX);
    if (f === null || !total) return;
    cancelResume();
    seek(f * total);
  };

  const cycleSpeed = () => {
    const next = SPEEDS[(SPEEDS.indexOf(speed) + 1) % SPEEDS.length];
    setSpeed(next);
    if (audioRef.current) audioRef.current.playbackRate = next;
  };

  const progress = total ? Math.min(1, current / total) : 0;

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-0 z-30 flex justify-center px-8 pb-6">
      <div className="pointer-events-auto w-full max-w-[680px] border border-hairline bg-panel/95 shadow-[0_10px_34px_rgba(0,0,0,0.18)] backdrop-blur-xl">
        <audio
          ref={audioRef}
          src={convertFileSrc(sourcePath)}
          preload="metadata"
          onPlay={() => setPlaying(true)}
          onPause={() => setPlaying(false)}
          onEnded={() => {
            setPlaying(false);
            cancelResume();
          }}
          onError={() => setFailed(true)}
        />

        {/* Fixed height. Every child sizes to this row rather than the row
            sizing to its children, so nothing here can inflate the bar. */}
        <div className="flex h-11 items-center gap-2 px-1.5">
          <button
            onClick={toggle}
            disabled={failed}
            aria-label={playing ? "Pause" : "Play"}
            className="relative flex h-8 w-8 shrink-0 items-center justify-center bg-ink text-surface transition-opacity hover:opacity-80 disabled:opacity-40"
          >
            {playing ? (
              <span className="flex gap-[3px]">
                <span className="block h-3 w-[3px] bg-current" />
                <span className="block h-3 w-[3px] bg-current" />
              </span>
            ) : (
              <span
                className="block h-0 w-0"
                style={{
                  borderTop: "5px solid transparent",
                  borderBottom: "5px solid transparent",
                  borderLeft: "8px solid currentColor",
                  marginLeft: 2,
                }}
              />
            )}
            {/* Auto-resume countdown, drawn as a draining underline so it
                doesn't need a number or steal attention from the text. */}
            {resumeIn !== null && (
              <span
                className="absolute inset-x-0 bottom-0 h-[3px] bg-sage transition-[width] duration-200"
                style={{ width: `${(resumeIn / RESUME_DELAY_MS) * 100}%` }}
              />
            )}
          </button>

          <div
            ref={scrubRef}
            onPointerDown={(e) => {
              e.currentTarget.setPointerCapture(e.pointerId);
              scrubTo(e.clientX);
            }}
            onPointerMove={(e) => {
              setHover(fractionAt(e.clientX));
              if (e.buttons === 1) scrubTo(e.clientX);
            }}
            onPointerLeave={() => setHover(null)}
            className="relative h-7 min-w-0 flex-1 cursor-pointer"
          >
            {peaks?.length ? (
              <Waveform peaks={peaks} progress={progress} hover={hover} />
            ) : (
              // Pre-waveform transcripts still need a usable scrubber.
              <div className="absolute inset-y-0 my-auto h-2 w-full bg-hairline-soft">
                <div
                  className="h-full bg-ink"
                  style={{ width: `${progress * 100}%` }}
                />
              </div>
            )}
            <div
              className="pointer-events-none absolute inset-y-0 w-px bg-amber"
              style={{ left: `${progress * 100}%` }}
            />
          </div>

          <span className="mono-data shrink-0 text-[10px] tabular-nums leading-none text-faint">
            <span className="text-ink">{formatTimestamp(current)}</span>
            {" / "}
            {formatTimestamp(total)}
          </span>

          {failed ? (
            <span className="micro text-amber">UNPLAYABLE</span>
          ) : (
            <button
              onClick={cycleSpeed}
              className="micro mono-data h-6 shrink-0 border border-hairline px-1.5 leading-none text-grey transition-colors hover:border-ink hover:text-ink"
            >
              {speed}×
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
