import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import gsap from "gsap";

import type { BriefCapability, MeetingCapability, Settings as Stored } from "../lib/api";
import { briefCapability } from "../lib/api";
import { EASE, prefersReducedMotion, useGsap } from "../lib/motion";
import { DEFAULT_CHORD, glyphs } from "../lib/shortcut";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

/**
 * Which chapters somebody has already been shown.
 *
 * A list rather than the boolean this used to be, and that is the whole point.
 * The old key answered "has this person seen the tutorial", which is the wrong
 * question the moment a version ships a feature the tutorial did not cover:
 * showing the tour again teaches four things they know to teach one they don't,
 * so in practice it never gets shown and the feature goes undiscovered.
 *
 * Keyed by chapter, the question becomes "which of these has this person not
 * seen", and a release that adds a chapter shows exactly that chapter.
 *
 * `localStorage` rather than the Rust settings file, following the rule at the
 * top of `settings.rs`: that file exists for settings the globe-key path has to
 * read from a thread with no window open. Nothing about a tutorial is ever read
 * from there.
 */
const SEEN_KEY = "voicedumps.tutorial.chapters";
/** The pre-chapter key. Its presence means "saw the tour as it was in 1.0". */
const OLD_KEY = "voicedumps.tutorial.seen";

/** Chapters that existed before chapter-tracking did. */
const IN_VERSION_ONE: ChapterKey[] = ["welcome", "dictate", "meeting", "done"];

export type ChapterKey =
  | "welcome"
  | "dictate"
  | "meeting"
  | "intelligence"
  | "ask"
  | "done";

/** Every chapter, in the order they are taught. */
export const CHAPTERS: ChapterKey[] = [
  "welcome",
  "dictate",
  "meeting",
  "intelligence",
  "ask",
  "done",
];

function readSeen(): ChapterKey[] {
  try {
    const raw = localStorage.getItem(SEEN_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) return parsed.filter((c) => CHAPTERS.includes(c));
    }
    // Upgrading from the boolean. They saw what 1.0 had, and nothing since.
    if (localStorage.getItem(OLD_KEY) === "1") return IN_VERSION_ONE;
  } catch {
    // A webview with storage disabled must not mean the tour every launch.
    return CHAPTERS;
  }
  return [];
}

function writeSeen(chapters: ChapterKey[]) {
  try {
    localStorage.setItem(SEEN_KEY, JSON.stringify(chapters));
  } catch {
    // Nothing to do: worst case it is offered again next launch.
  }
}

/**
 * What this person has not been shown yet, in teaching order.
 *
 * Empty means there is nothing to say and the tutorial does not open at all.
 */
export function unseenChapters(): ChapterKey[] {
  const seen = readSeen();
  const fresh = CHAPTERS.filter((c) => !seen.includes(c));

  // Nothing new, or nothing but the bookends. "Welcome" and "That is the whole
  // app" are framing for a first run; on their own, after an upgrade, they are
  // a tour of nothing.
  const substantive = fresh.filter((c) => c !== "welcome" && c !== "done");
  if (substantive.length === 0) return [];

  // A returning user gets the new chapters and a closing card, without being
  // welcomed to an app they have been using for a month.
  const firstRun = seen.length === 0;
  return firstRun ? CHAPTERS : [...substantive, "done"];
}

/** For the Settings row that replays it: everything, regardless of history. */
export function everyChapter(): ChapterKey[] {
  return CHAPTERS;
}

export function markSeen(chapters: ChapterKey[]) {
  const already = readSeen();
  writeSeen([...new Set([...already, ...chapters])]);
}

type Props = {
  settings: Stored | null;
  meeting: MeetingCapability | null;
  /** Which chapters to show. Comes from `unseenChapters` or `everyChapter`. */
  chapters: ChapterKey[];
  onDone: () => void;
};

/** A key cap, drawn rather than described. */
function Cap({ label }: { label: string }) {
  return (
    <kbd className="mono-data inline-flex min-w-[32px] items-center justify-center border border-hairline bg-panel px-2 py-1.5 text-[13px] text-ink">
      {label}
    </kbd>
  );
}

/**
 * A line of the app's own square glyphs, marching.
 *
 * The tutorial's one piece of ornament, and it earns its place by being the
 * same vocabulary the rest of the app is drawn in — the sidebar's bullets, the
 * dictation pill's meter. A stock illustration would be the first thing in this
 * app that came from somewhere else.
 */
function Marchers({ count = 7, lit = 0 }: { count?: number; lit?: number }) {
  return (
    <div className="flex items-center gap-1.5" aria-hidden>
      {Array.from({ length: count }, (_, i) => (
        <span
          key={i}
          data-march
          className={[
            "h-[9px] w-[9px] transition-colors duration-500",
            i < lit ? "bg-sage-dim" : "bg-hairline",
          ].join(" ")}
        />
      ))}
    </div>
  );
}

/**
 * The walkthrough.
 *
 * Modelled on the one thing dictation apps get right about onboarding: the
 * shortcut chapter is not a screenshot of a key, it waits for the user to
 * actually press it. A shortcut you have used once is learned; a shortcut you
 * have read about is a thing to look up later.
 *
 * Everything here is skippable and re-openable from Settings, because the same
 * screen that teaches a new user is in the way of one who already knows.
 */
export function Onboarding({ settings, meeting, chapters, onDone }: Props) {
  const [at, setAt] = useState(0);
  /** Set once the user has held the chord and said something. */
  const [dictated, setDictated] = useState(false);
  const [listening, setListening] = useState(false);
  const [brain, setBrain] = useState<BriefCapability | null>(null);
  const advanced = useRef(false);

  /**
   * The chord this person actually dictates with, drawn on the key caps.
   *
   * Whatever they have set, not the globe: somebody who moved dictation to
   * ⌃⌥ and then upgrades must be told to hold ⌃⌥, or the one chapter that
   * asks them to press a key is asking for the wrong one.
   *
   * The fallback comes from `shortcut.ts` rather than being spelled here. It
   * used to read `?? "fn"`, which is what the key is *printed* as and not what
   * it is *called* — `parse("fn")` matches nothing, so `glyphs` returned an
   * empty string and the heading read "Hold  and talk" with a hole in it for
   * the first second of every fresh install, before the settings arrived.
   */
  const chord = settings?.shortcut ?? DEFAULT_CHORD;
  const chapter = chapters[at] ?? "done";

  // Asked when the chapter comes up rather than on mount: somebody can switch
  // Apple Intelligence on in the middle of the tour, and the honest thing is to
  // check at the moment we are about to make a claim about it.
  useEffect(() => {
    if (chapter !== "intelligence") return;
    let cancelled = false;
    briefCapability()
      .then((b) => !cancelled && setBrain(b))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [chapter]);

  // Watch real dictation while the shortcut chapter is on screen. This is the
  // whole point of it: the app confirms the key worked, so nobody leaves
  // onboarding unsure whether it did.
  useEffect(() => {
    if (chapter !== "dictate") return;
    const sub = listen<"idle" | "recording" | "transcribing">(
      "dictation-state",
      (e) => {
        if (e.payload === "recording") setListening(true);
        // "Transcribing" is the honest proof: audio was captured and handed to
        // the engine. Waiting for the paste instead would make the chapter
        // depend on whatever app happens to have focus.
        if (e.payload === "transcribing") {
          setListening(false);
          setDictated(true);
        }
        if (e.payload === "idle") setListening(false);
      },
    );
    return () => {
      sub.then((un) => un()).catch(() => {});
    };
  }, [chapter]);

  const next = useCallback(() => {
    setAt((i) => Math.min(i + 1, chapters.length - 1));
  }, [chapters.length]);

  // Give the success a beat to register before moving on, rather than yanking
  // the screen away the instant it goes green.
  useEffect(() => {
    if (!dictated || advanced.current) return;
    advanced.current = true;
    const t = setTimeout(next, 1100);
    return () => clearTimeout(t);
  }, [dictated, next]);

  const finish = useCallback(() => {
    // Everything shown this run is now seen — including chapters skipped past,
    // because skipping is a decision about a thing you were offered.
    markSeen(chapters);
    onDone();
  }, [chapters, onDone]);

  /**
   * The entrance, as a timeline rather than a single tween.
   *
   * Each chapter arrives in the order it is meant to be read: the counter, then
   * the heading, then the body, then whatever the chapter is actually about,
   * then the button. It is about 600ms end to end — long enough to feel
   * composed, short enough that the second time through nobody is waiting for
   * it.
   *
   * The squares march in on their own stagger because they are the one thing on
   * screen that is not text, and giving them the same curve as a paragraph
   * would waste them.
   */
  const scope = useGsap(
    ({ scope }) => {
      if (prefersReducedMotion()) return;
      const tl = gsap.timeline();
      tl.fromTo(
        scope.querySelectorAll("[data-rise]"),
        { opacity: 0, y: 14 },
        { opacity: 1, y: 0, duration: 0.42, ease: EASE.snap, stagger: 0.07 },
      );
      const marchers = scope.querySelectorAll("[data-march]");
      if (marchers.length) {
        tl.fromTo(
          marchers,
          { opacity: 0, scale: 0.4 },
          {
            opacity: 1,
            scale: 1,
            duration: 0.3,
            ease: EASE.snap,
            stagger: 0.045,
          },
          "-=0.3",
        );
      }
    },
    [chapter],
  );

  const heading = "text-[30px] font-medium leading-[1.15] tracking-[-0.02em] text-ink";
  const body = "text-[15px] leading-[1.65] text-grey";
  const aside = "text-[13px] leading-relaxed text-faint";
  const primary =
    "micro border border-ink bg-ink px-5 py-3 text-surface transition-colors hover:bg-transparent hover:text-ink";
  const quiet =
    "micro border border-hairline px-4 py-2.5 text-faint transition-colors hover:border-ink hover:text-ink";

  return (
    <div className="dot-grid flex h-full flex-col bg-surface">
      <div
        ref={scope}
        className="titlebar-pad drag-region flex h-full flex-col items-center justify-center px-10"
      >
        <div className="no-drag w-full max-w-[560px]">
          {/* Where you are, in the app's own counting idiom. */}
          <div data-rise className="flex items-center justify-between">
            <p className="eyebrow text-faint">
              {String(at + 1).padStart(2, "0")} //{" "}
              {String(chapters.length).padStart(2, "0")}
              {chapters[0] !== "welcome" && " · WHAT'S NEW"}
            </p>
            <button
              onClick={finish}
              className="micro text-faint transition-colors hover:text-ink"
            >
              SKIP
            </button>
          </div>

          <div data-rise className="mt-3 flex gap-px">
            {chapters.map((c, i) => (
              <span
                key={c}
                className={[
                  "h-1 flex-1 transition-colors duration-300",
                  i <= at ? "bg-sage-dim" : "bg-hairline",
                ].join(" ")}
              />
            ))}
          </div>

          {chapter === "welcome" && (
            <>
              <div data-rise className="mt-9">
                <Marchers count={7} lit={7} />
              </div>
              <h1 data-rise className={`mt-6 ${heading}`}>
                Everything here runs
                <br />
                on this Mac
              </h1>
              <p data-rise className={`mt-5 ${body}`}>
                Speech never leaves the machine. No account, no API key, nothing
                to sign in to — the model is on your disk and the work happens on
                your own processor.
              </p>
              <div data-rise className="mt-8">
                <button onClick={next} className={primary}>
                  SHOW ME
                </button>
              </div>
            </>
          )}

          {chapter === "dictate" && (
            <>
              <h1 data-rise className={`mt-9 ${heading}`}>
                Hold {glyphs(chord)} and talk
              </h1>
              <p data-rise className={`mt-5 ${body}`}>
                In any app. Hold the key, say a sentence, let go — the text is
                typed where your cursor is. Try it right now, even here.
              </p>

              <div
                data-rise
                className={[
                  "mt-7 flex items-center gap-4 border bg-panel px-5 py-4 transition-colors duration-300",
                  dictated
                    ? "border-sage-dim"
                    : listening
                      ? "border-amber"
                      : "border-hairline",
                ].join(" ")}
              >
                <span className={dictated ? "text-sage-dim" : "text-faint"}>
                  <PixelCluster
                    pattern={CLUSTERS.brand}
                    size={8}
                    gap={3}
                    pulse={listening}
                  />
                </span>
                <div className="min-w-0">
                  <p className="micro text-ink">
                    {dictated
                      ? "THAT WAS IT — YOU KNOW HOW NOW"
                      : listening
                        ? "LISTENING…"
                        : "WAITING FOR THE KEY"}
                  </p>
                  <p className={`mt-1.5 ${aside}`}>
                    {dictated
                      ? "Your words are being transcribed."
                      : "Nothing is recorded until the key is held."}
                  </p>
                </div>
              </div>

              <div data-rise className="mt-5 flex items-center gap-3 text-faint">
                <Cap label={glyphs(chord)} />
                <span className="micro">HOLD // SPEAK // RELEASE</span>
              </div>

              <p data-rise className={`mt-5 ${aside}`}>
                Nothing happening? macOS has to allow VoiceDumps to watch for
                that key, under Privacy &amp; Security › Accessibility.{" "}
                <button
                  onClick={() => invoke("open_accessibility_settings").catch(() => {})}
                  className="underline underline-offset-2 transition-colors hover:text-ink"
                >
                  Open that pane
                </button>
                .
              </p>

              <div data-rise className="mt-7">
                <button onClick={next} className={quiet}>
                  I'LL TRY LATER
                </button>
              </div>
            </>
          )}

          {chapter === "meeting" && (
            <>
              <h1 data-rise className={`mt-9 ${heading}`}>
                Record a call,
                <br />
                both sides
              </h1>
              <p data-rise className={`mt-5 ${body}`}>
                VoiceDumps captures your microphone and whatever this Mac is
                playing as two separate tracks, so the transcript knows who said
                what without guessing at voices.
              </p>

              {/* The two tracks, drawn. Cheaper to understand than the sentence
                  above it, and it is the one idea in this chapter. */}
              <div data-rise className="mt-7 border border-hairline bg-panel">
                {[
                  ["YOU", "your microphone", "bg-sage-dim"],
                  ["OTHERS", "what the Mac plays", "bg-amber"],
                ].map(([who, what, tone], row) => (
                  <div
                    key={who}
                    className={[
                      "flex items-center gap-4 px-5 py-3.5",
                      row > 0 ? "border-t border-hairline-soft" : "",
                    ].join(" ")}
                  >
                    <span className="micro w-[62px] shrink-0 text-ink">{who}</span>
                    <span className="flex flex-1 items-center gap-[3px]" aria-hidden>
                      {Array.from({ length: 22 }, (_, i) => (
                        <span
                          data-march
                          key={i}
                          className={`${tone} w-full`}
                          style={{
                            // A fixed pseudo-waveform: the same every run, so
                            // the picture is a diagram and not a toy.
                            height: `${4 + ((i * (row ? 7 : 5)) % 11)}px`,
                          }}
                        />
                      ))}
                    </span>
                    <span className={`${aside} w-[120px] shrink-0 text-right`}>
                      {what}
                    </span>
                  </div>
                ))}
              </div>

              <p data-rise className={`mt-5 ${aside}`}>
                No bot joins the call. Zoom, Meet, Teams and a phone on speaker
                all work the same, because it is the audio that is captured, not
                the app.
              </p>

              {meeting && !meeting.available && meeting.reason && (
                <p
                  data-rise
                  className="mt-4 border border-hairline bg-panel px-4 py-3 text-[13px] leading-relaxed text-grey"
                >
                  {meeting.reason}
                </p>
              )}

              <div data-rise className="mt-7">
                <button onClick={next} className={primary}>
                  NEXT
                </button>
              </div>
            </>
          )}

          {chapter === "intelligence" && (
            <>
              <h1 data-rise className={`mt-9 ${heading}`}>
                Names, summaries
                <br />
                and subjects
              </h1>
              <p data-rise className={`mt-5 ${body}`}>
                Apple's own model runs on this Mac and gives every recording a
                real name, an overview, and the subjects it was about. Nothing is
                uploaded and nothing reaches us.
              </p>

              <div
                data-rise
                className={[
                  "mt-7 border bg-panel px-5 py-4 transition-colors duration-300",
                  brain === null
                    ? "border-hairline"
                    : brain.available
                      ? "border-sage-dim"
                      : "border-amber",
                ].join(" ")}
              >
                <div className="flex items-center justify-between gap-4">
                  <span className="micro text-ink">APPLE INTELLIGENCE</span>
                  <span
                    className={[
                      "micro",
                      brain === null
                        ? "text-faint"
                        : brain.available
                          ? "text-sage-dim"
                          : "text-amber",
                    ].join(" ")}
                  >
                    {brain === null ? "CHECKING…" : brain.available ? "ON" : "OFF"}
                  </span>
                </div>
                {/* The backend's own sentence when it is off, so this screen and
                    a blocked Overview pane can never disagree about why. */}
                <p className={`mt-2.5 ${aside}`}>
                  {brain === null
                    ? "Asking macOS…"
                    : brain.available
                      ? "Working. Your recordings will be named and summarised here."
                      : brain.message}
                </p>
              </div>

              {/* The part that matters most, and the reason this chapter is not
                  a gate: everything else works either way. */}
              {/* Nothing until the answer is in. `brain?.available` is falsy
                  while the check is still running, and the version of this that
                  read it directly told people their Apple Intelligence was off
                  a half-second before discovering it was on. */}
              {brain !== null && (
                <p data-rise className={`mt-5 ${aside}`}>
                  {brain.available
                    ? "Switch it off in System Settings whenever you like — recording, transcription and search carry on regardless."
                    : "You can turn it on later in System Settings › Apple Intelligence & Siri. Until then, recording, transcription, dictation and search all work exactly as they do now — you simply keep your own titles."}
                </p>
              )}

              <div data-rise className="mt-7">
                <button onClick={next} className={primary}>
                  NEXT
                </button>
              </div>
            </>
          )}

          {chapter === "ask" && (
            <>
              <h1 data-rise className={`mt-9 ${heading}`}>
                Ask your notes
                <br />
                a question
              </h1>
              <p data-rise className={`mt-5 ${body}`}>
                Not a search box — search is already there. This reads the
                recordings that bear on your question and answers from them,
                citing the ones it used so you can open them and check.
              </p>

              {/* Three real questions. Concrete beats describing a capability:
                  the point lands when somebody recognises a question they have
                  actually had. */}
              <div data-rise className="mt-7 border border-hairline bg-panel">
                {[
                  "what did we decide about pricing",
                  "what are my action items",
                  "what did I say about the launch last week",
                ].map((q, i) => (
                  <div
                    key={q}
                    className={[
                      "flex items-baseline gap-3 px-5 py-3",
                      i > 0 ? "border-t border-hairline-soft" : "",
                    ].join(" ")}
                  >
                    <span className="mt-[3px] shrink-0 text-faint">
                      <PixelCluster pattern={CLUSTERS.bullet} size={2.5} />
                    </span>
                    <span className="text-[14px] leading-relaxed text-ink">
                      {q}
                    </span>
                  </div>
                ))}
              </div>

              <p data-rise className={`mt-5 ${aside}`}>
                Ask by typing or by speaking, and follow up in plain
                language — "write that as a paragraph", "make it shorter". It
                needs Apple Intelligence; without it, the same button still finds
                and shows you the recordings that match.
              </p>

              <div data-rise className="mt-7">
                <button onClick={next} className={primary}>
                  NEXT
                </button>
              </div>
            </>
          )}

          {chapter === "done" && (
            <>
              <div data-rise className="mt-9">
                <Marchers count={7} lit={7} />
              </div>
              <h1 data-rise className={`mt-6 ${heading}`}>
                {chapters[0] === "welcome" ? "That is the whole app" : "That is all of it"}
              </h1>
              <div data-rise className="mt-6 border border-hairline bg-panel">
                {[
                  ["DICTATE", `Hold ${glyphs(chord)} in any app`],
                  ["TRANSCRIBE", "Drop an audio or video file in"],
                  ["MEET", "Record a call from the start screen"],
                  ["ASK", "Put a question to everything you have said"],
                  ["FIND", "All of it is searchable"],
                ].map(([label, note], i) => (
                  <div
                    key={label}
                    className={[
                      "flex items-baseline justify-between gap-4 px-5 py-3",
                      i > 0 ? "border-t border-hairline-soft" : "",
                    ].join(" ")}
                  >
                    <span className="micro text-ink">{label}</span>
                    <span className="text-[13px] text-grey">{note}</span>
                  </div>
                ))}
              </div>
              <p data-rise className={`mt-5 ${aside}`}>
                This walkthrough is in Settings whenever you want it again.
              </p>
              <div data-rise className="mt-7">
                <button onClick={finish} className={primary}>
                  START USING VOICEDUMPS
                </button>
              </div>
            </>
          )}

        </div>
      </div>
    </div>
  );
}
