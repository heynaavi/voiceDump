import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

type State = "idle" | "recording" | "transcribing";

/**
 * Floating status for globe-key dictation.
 *
 * Dictation happens while another app has focus, so this is mostly a
 * confirmation you can glance at — and, more importantly, the only place a
 * permission failure can surface. Without it the feature fails completely
 * silently, which is exactly what happened the first time.
 */
export function DictationPill() {
  const [state, setState] = useState<State>("idle");
  const [error, setError] = useState<string | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const started = useRef(0);

  useEffect(() => {
    const subs = [
      listen<State>("dictation-state", (e) => {
        setState(e.payload);
        if (e.payload === "recording") {
          started.current = Date.now();
          setError(null);
        }
      }),
      // An empty payload clears — sent once permission is granted.
      listen<string>("dictation-error", (e) => setError(e.payload || null)),
    ];
    return () => {
      subs.forEach((s) => s.then((un) => un()).catch(() => {}));
    };
  }, []);

  useEffect(() => {
    if (state !== "recording") return;
    const t = setInterval(
      () => setElapsed((Date.now() - started.current) / 1000),
      200,
    );
    return () => clearInterval(t);
  }, [state]);

  if (error) {
    return (
      <div className="pointer-events-auto absolute bottom-4 left-1/2 z-40 w-[420px] -translate-x-1/2 border border-amber bg-panel">
        <p className="micro border-b border-hairline-soft bg-amber px-3 py-1.5 text-surface">
          DICTATION UNAVAILABLE
        </p>
        <p className="px-3 py-2.5 text-[11px] leading-relaxed text-grey">{error}</p>
        <div className="flex justify-end gap-px border-t border-hairline-soft px-3 py-2">
          <button
            onClick={() => setError(null)}
            className="micro border border-hairline px-2.5 py-1.5 text-faint transition-colors hover:border-ink hover:text-ink"
          >
            DISMISS
          </button>
          <button
            onClick={() => invoke("open_accessibility_settings").catch(() => {})}
            className="micro border border-ink bg-ink px-2.5 py-1.5 text-surface transition-colors hover:bg-transparent hover:text-ink"
          >
            OPEN SETTINGS
          </button>
        </div>
      </div>
    );
  }

  if (state === "idle") return null;

  return (
    <div className="pointer-events-none absolute bottom-4 left-1/2 z-40 -translate-x-1/2">
      <div className="flex items-center gap-2.5 border border-ink bg-panel px-3 py-2 shadow-[0_8px_24px_rgba(0,0,0,0.2)]">
        <span
          className={[
            "block h-2.5 w-2.5",
            state === "recording" ? "animate-pulse bg-amber" : "bg-sage-dim",
          ].join(" ")}
        />
        <span className="micro text-ink">
          {state === "recording" ? "LISTENING" : "TRANSCRIBING"}
        </span>
        {state === "recording" && (
          <span className="mono-data text-[11px] tabular-nums text-faint">
            {elapsed.toFixed(1)}s
          </span>
        )}
        <span className="micro text-faint">
          {state === "recording" ? "GLOBE TO STOP" : ""}
        </span>
      </div>
    </div>
  );
}
