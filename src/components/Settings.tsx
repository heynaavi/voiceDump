import { useEffect, useRef, useState } from "react";
import gsap from "gsap";

import type { Mic, Settings as Stored } from "../lib/api";
import { listMicrophones } from "../lib/api";
import { EASE, prefersReducedMotion } from "../lib/motion";
import {
  glyphs,
  held,
  isModifier,
  refusal,
  toChord,
  words,
  type Key,
} from "../lib/shortcut";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

type Props = {
  settings: Stored | null;
  onLivePreview: (enabled: boolean) => void;
  onMicrophone: (name: string | null) => void;
  onShortcut: (chord: string) => Promise<void>;
};

/**
 * A group of related settings.
 *
 * Same frame as an Insights panel — bordered, on `bg-panel`, eyebrow heading —
 * because these are two halves of one full-pane vocabulary and there is no
 * reason for them to look like different applications.
 */
function Group({
  title,
  aside,
  children,
}: {
  title: string;
  aside?: string;
  children: React.ReactNode;
}) {
  return (
    <section data-row className="border border-hairline bg-panel">
      <div className="flex items-baseline justify-between gap-3 border-b border-hairline px-4 py-3">
        <h2 className="eyebrow text-ink">{title}</h2>
        {aside && <span className="micro shrink-0 text-faint">{aside}</span>}
      </div>
      {children}
    </section>
  );
}

/**
 * One setting: what it is on the left, the control on the right, and the
 * sentence that explains it underneath rather than hidden in a tooltip. The
 * sidebar rows these replace had to earn every pixel; a pane does not.
 */
function Row({
  label,
  note,
  control,
}: {
  label: string;
  note: string;
  control: React.ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-6 px-4 py-3.5">
      <div className="min-w-0">
        <p className="text-[13px] text-ink">{label}</p>
        <p className="mt-1 max-w-[46ch] text-[12px] leading-relaxed text-grey">
          {note}
        </p>
      </div>
      <div className="shrink-0 pt-0.5">{control}</div>
    </div>
  );
}

/** The same 7px swatch the theme control uses: filled means on. */
function Switch({
  on,
  onClick,
  disabled,
}: {
  on: boolean | null;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      aria-pressed={on === true}
      className="group flex items-center gap-1.5 border border-hairline px-2.5 py-1.5 transition-colors hover:border-sage-dim disabled:opacity-50"
    >
      <span
        className={[
          "h-[7px] w-[7px] border",
          on ? "border-ink bg-ink" : "border-hairline bg-transparent",
        ].join(" ")}
      />
      {/* `text-ink!`: .diagnostic sets its own colour and is declared after
          Tailwind's utilities, so a plain colour class on it never lands. */}
      <span className={`diagnostic ${on ? "text-ink!" : ""}`}>
        {on === null ? "—" : on ? "ON" : "OFF"}
      </span>
    </button>
  );
}

/**
 * Records the keys you hold to dictate.
 *
 * Listening, not typing: the recorder waits for a set of modifiers to go down
 * and commits when they come back up, which is the same gesture as using the
 * shortcut. The globe key cannot be recorded — WKWebView never reports it —
 * so it is a button rather than something to press.
 */
function ShortcutRecorder({
  chord,
  onChoose,
}: {
  chord: string;
  onChoose: (chord: string) => Promise<void>;
}) {
  const [recording, setRecording] = useState(false);
  const [pressed, setPressed] = useState<Key[]>([]);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    if (!recording) return;

    const down = (e: KeyboardEvent) => {
      // Nothing typed while recording should reach the window underneath.
      e.preventDefault();
      if (e.key === "Escape") {
        setRecording(false);
        return;
      }
      if (!isModifier(e)) {
        setProblem(
          "Only modifier keys can be the chord — the keyboard is watched without being taken over, so a letter would also reach whatever you are dictating into.",
        );
        return;
      }
      setProblem(null);
      setPressed(held(e));
    };

    const up = (e: KeyboardEvent) => {
      e.preventDefault();
      // Commit when the last key comes up, which is the same motion as
      // finishing a dictation — hold, then let go.
      if (held(e).length > 0) return;
      setPressed((keys) => {
        if (keys.length === 0) return keys;
        const no = refusal(keys);
        if (no) {
          setProblem(no);
          return [];
        }
        setRecording(false);
        onChoose(toChord(keys)).catch((err) => setProblem(String(err)));
        return [];
      });
    };

    window.addEventListener("keydown", down, true);
    window.addEventListener("keyup", up, true);
    return () => {
      window.removeEventListener("keydown", down, true);
      window.removeEventListener("keyup", up, true);
    };
  }, [recording, onChoose]);

  const showing = pressed.length ? toChord(pressed) : chord;

  return (
    <div className="flex flex-col items-end gap-2">
      <div className="flex items-center gap-2">
        {chord !== "globe" && !recording && (
          <button
            onClick={() => onChoose("globe").catch((e) => setProblem(String(e)))}
            className="micro border border-hairline px-2.5 py-1.5 text-grey transition-colors hover:border-sage-dim hover:text-ink"
          >
            USE GLOBE
          </button>
        )}
        <button
          onClick={() => {
            setProblem(null);
            setPressed([]);
            setRecording((r) => !r);
          }}
          aria-pressed={recording}
          title={recording ? "Press Escape to cancel" : words(chord)}
          className={`flex min-w-[128px] items-center justify-center gap-2 border px-3 py-1.5 transition-colors ${
            recording
              ? "border-sage-dim bg-sage-dim/15"
              : "border-hairline hover:border-sage-dim"
          }`}
        >
          <span className="text-[15px] leading-none text-ink">
            {glyphs(showing) || "—"}
          </span>
          {recording && (
            <span className="text-sage-dim">
              <PixelCluster pattern={CLUSTERS.brand} size={2.5} pulse />
            </span>
          )}
        </button>
      </div>

      <p className="micro max-w-[30ch] text-right text-faint">
        {recording
          ? pressed.length
            ? "RELEASE TO SET"
            : "HOLD YOUR KEYS // ESC TO CANCEL"
          : words(chord).toUpperCase()}
      </p>

      {problem && (
        <p className="max-w-[36ch] text-right text-[12px] leading-relaxed text-amber">
          {problem}
        </p>
      )}
    </div>
  );
}

/** Which microphone gets recorded. A list rather than the sidebar's popover:
 *  there is room here, and seeing the alternatives is the point of the pane. */
function Microphones({
  chosen,
  onChoose,
}: {
  chosen: string | null;
  onChoose: (name: string | null) => void;
}) {
  const [mics, setMics] = useState<Mic[]>([]);

  // Re-enumerated whenever the pane opens: someone coming here has often just
  // plugged something in.
  useEffect(() => {
    listMicrophones()
      .then(setMics)
      .catch(() => setMics([]));
  }, []);

  const systemName = mics.find((m) => m.is_default)?.name ?? null;
  const missing =
    chosen !== null && mics.length > 0 && !mics.some((m) => m.name === chosen);

  const options: { name: string | null; label: string; hint?: string }[] = [
    { name: null, label: "System default", hint: systemName ?? undefined },
    ...mics.map((m) => ({ name: m.name as string | null, label: m.name })),
    ...(missing ? [{ name: chosen, label: chosen!, hint: "Not connected" }] : []),
  ];

  if (mics.length === 0 && !missing) {
    return <p className="micro px-4 py-3.5 text-faint">NO MICROPHONE FOUND</p>;
  }

  return (
    <ul>
      {options.map((o) => {
        const active = chosen === o.name;
        const absent = missing && o.name === chosen;
        return (
          <li key={o.name ?? "system"}>
            <button
              onClick={() => onChoose(o.name)}
              className="flex w-full items-start gap-2.5 px-4 py-2.5 text-left transition-colors hover:bg-rail"
            >
              <span
                className={[
                  "mt-[5px] h-[7px] w-[7px] shrink-0 border",
                  absent
                    ? "border-amber bg-amber"
                    : active
                      ? "border-ink bg-ink"
                      : "border-hairline bg-transparent",
                ].join(" ")}
              />
              <span className="min-w-0">
                <span
                  className={`block truncate text-[13px] ${active ? "text-ink" : "text-grey"}`}
                >
                  {o.label}
                </span>
                {o.hint && (
                  <span
                    className={`micro mt-0.5 block truncate ${absent ? "text-amber!" : "text-faint"}`}
                  >
                    {o.hint}
                  </span>
                )}
              </span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

export function Settings({
  settings,
  onLivePreview,
  onMicrophone,
  onShortcut,
}: Props) {
  const sheet = useRef<HTMLDivElement>(null);

  // The groups arrive in reading order, matching Insights. Keyed on the
  // settings landing rather than on mount, so the entrance doesn't play against
  // an empty pane and finish before there is anything to see.
  useEffect(() => {
    if (!settings || !sheet.current) return;
    if (prefersReducedMotion()) return;
    const rows = sheet.current.querySelectorAll("[data-row]");
    const tl = gsap.timeline();
    tl.fromTo(
      rows,
      { opacity: 0, y: 14 },
      { opacity: 1, y: 0, duration: 0.42, ease: EASE.snap, stagger: 0.055 },
    );
    return () => {
      tl.kill();
      gsap.set(rows, { clearProps: "opacity,transform" });
    };
  }, [settings]);

  if (!settings) {
    return (
      <div className="titlebar-pad flex h-full flex-col items-center justify-center gap-3 p-8">
        <span className="text-sage-dim">
          <PixelCluster pattern={CLUSTERS.brand} size={7} gap={3} pulse />
        </span>
        <p className="micro text-faint">READING YOUR SETTINGS…</p>
      </div>
    );
  }

  return (
    <div className="titlebar-pad scroll-slim h-full overflow-y-auto">
      <div ref={sheet} className="mx-auto max-w-[640px] space-y-3 p-6">
        <header data-row>
          <h1 className="text-[22px] font-semibold tracking-[-0.01em] text-ink">
            Settings
          </h1>
          <p className="eyebrow mt-1 text-faint">
            KEPT ON THIS MAC // READ BY THE DICTATION KEY
          </p>
        </header>

        <Group title="DICTATION">
          <Row
            label="Shortcut"
            note="Hold these keys to record, let go to stop. Modifiers only — the keyboard is watched, not taken over, so a letter would also reach whatever you are dictating into."
            control={
              <ShortcutRecorder chord={settings.shortcut} onChoose={onShortcut} />
            }
          />
          <div className="border-t border-hairline" />
          <Row
            label="Live preview"
            note="Draft your words in the overlay while you are still speaking. It runs on the fast model, so it makes mistakes the pasted text will not."
            control={
              <Switch
                on={settings.live_preview}
                onClick={() => onLivePreview(!settings.live_preview)}
              />
            }
          />
        </Group>

        <Group
          title="INPUT"
          aside={settings.microphone ? "PINNED" : "FOLLOWING SYSTEM"}
        >
          <Microphones chosen={settings.microphone} onChoose={onMicrophone} />
        </Group>

        <p className="micro pb-2 text-faint">
          STORED IN SETTINGS.JSON BESIDE YOUR HISTORY // NOTHING IS UPLOADED
        </p>
      </div>
    </div>
  );
}
