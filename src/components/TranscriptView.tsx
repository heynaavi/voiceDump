import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import gsap from "gsap";

import type { Paragraph, Transcript } from "../lib/api";
import {
  archiveTranscriptMedia,
  exportPdf,
  fetchPeaks,
  setTranscriptPeaks,
  writeTextFile,
} from "../lib/api";
import { reconcileWords } from "../lib/diff";
import { fileName, formatDuration } from "../lib/format";
import { EASE, prefersReducedMotion } from "../lib/motion";
import { AudioPlayer, type PlayerHandle } from "./AudioPlayer";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

type Props = {
  transcript: Transcript;
  onRename: (id: string, title: string) => void;
  onDelete: (id: string) => void;
  onEdit: (id: string, paragraphs: Paragraph[]) => void;
  /** The AI is currently generating this note's title. */
  naming?: boolean;
};

/** How long the clicked word stays lit before the caret takes over. */
const FLASH_MS = 420;

/** Type scale for the focused paragraph vs the rest. */
const FOCUS_SIZE = 19.5;
const REST_SIZE = 17;
const REST_OPACITY = 0.45;

/**
 * Vertical space the floating player occupies, plus breathing room. The player
 * overlays the text rather than displacing it, so every scroll calculation has
 * to treat the bottom of the viewport as being this much higher than it is.
 */
const PLAYER_SAFE_PX = 96;
/** Where a paragraph's top lands when it has to be scrolled into view. */
const READING_LINE = 0.2;

export function TranscriptView({ transcript, onRename, onDelete, onEdit, naming = false }: Props) {
  const [copied, setCopied] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const [title, setTitle] = useState(transcript.title);
  const [time, setTime] = useState(0);
  const [paragraphs, setParagraphs] = useState<Paragraph[]>([]);
  const [editing, setEditing] = useState<number | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const [peaks, setPeaks] = useState<number[] | null>(null);
  const [focus, setFocus] = useState(0);
  const [mediaPath, setMediaPath] = useState(transcript.source_path);

  const scrollRef = useRef<HTMLDivElement>(null);
  const playerRef = useRef<PlayerHandle | null>(null);
  const activeWordRef = useRef<HTMLSpanElement | null>(null);
  const editRef = useRef<HTMLParagraphElement | null>(null);
  const paraRefs = useRef<(HTMLParagraphElement | null)[]>([]);
  // Set while the focus tween is reflowing text, so the word-follow scroll
  // doesn't fight the size animation for control of scrollTop.
  const tweening = useRef(false);
  // Where the click landed, so the caret can be dropped in the same spot once
  // the paragraph swaps from word spans to editable text.
  const caretPoint = useRef<{ x: number; y: number } | null>(null);
  const flashTimer = useRef<number | null>(null);

  // The heavy reset (scroll, focus, paragraphs) belongs to *switching* notes,
  // keyed on id. The title is synced on its own so an AI rename landing mid-read
  // updates the header without yanking the reader back to the top or resetting
  // playback.
  // True only when the title changed for the note already on screen (an AI
  // rename), so the header reveal doesn't fire on every note-open.
  const seenTitle = useRef<{ id: string; title: string }>({ id: "", title: "" });
  const titleRenamed =
    seenTitle.current.id === transcript.id &&
    seenTitle.current.title !== transcript.title;

  const seededId = useRef<string | null>(null);
  useEffect(() => {
    setTitle(transcript.title);
    seenTitle.current = { id: transcript.id, title: transcript.title };
    if (seededId.current === transcript.id) return;
    seededId.current = transcript.id;
    setTime(0);
    setEditing(null);
    setFlash(null);
    setFocus(0);
    setPeaks(transcript.peaks?.length ? transcript.peaks : null);
    setMediaPath(transcript.source_path);
    setParagraphs(
      transcript.paragraphs?.length
        ? transcript.paragraphs
        : [
            {
              start: 0,
              end: transcript.duration,
              text: transcript.text,
              words: [],
              edited: true,
            },
          ],
    );
    scrollRef.current?.scrollTo({ top: 0 });
  }, [transcript]);

  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(false), 1600);
    return () => clearTimeout(t);
  }, [copied]);

  useEffect(
    () => () => {
      if (flashTimer.current !== null) clearTimeout(flashTimer.current);
    },
    [],
  );

  // Transcripts saved before the waveform existed have no peaks. Compute them
  // once from the source file rather than making the user re-transcribe, and
  // write them back so it only ever happens once per transcript.
  useEffect(() => {
    if (transcript.peaks?.length) return;
    let cancelled = false;
    fetchPeaks(transcript.source_path)
      .then((p) => {
        if (cancelled || !p.length) return;
        setPeaks(p);
        return setTranscriptPeaks(transcript.id, p);
      })
      .catch(() => {
        // A missing or moved source file just means no waveform.
      });
    return () => {
      cancelled = true;
    };
  }, [transcript.id, transcript.source_path, transcript.peaks]);

  // Transcripts saved before the media library point at wherever their file
  // happened to live, in whatever format it happened to be — an Opus voice note,
  // among them, which the webview can't decode. Pull them in on first open.
  useEffect(() => {
    if (transcript.source_path.includes("/media/")) return;
    let cancelled = false;
    archiveTranscriptMedia(transcript.id)
      .then((p) => {
        if (!cancelled) setMediaPath(p);
      })
      .catch(() => {
        // Original gone or unreadable — keep the existing path.
      });
    return () => {
      cancelled = true;
    };
  }, [transcript.id, transcript.source_path]);

  const hasWords = paragraphs.some((p) => p.words?.length);

  // One flat, time-ordered word list makes "which word is playing" a single
  // binary search instead of a scan over every paragraph on each frame.
  const flatWords = useMemo(() => {
    const all: { start: number; end: number; key: string }[] = [];
    paragraphs.forEach((p, pi) => {
      if (p.edited) return;
      (p.words ?? []).forEach((w, wi) => {
        all.push({ start: w.start, end: w.end, key: `${pi}:${wi}` });
      });
    });
    return all;
  }, [paragraphs]);

  const activeKey = useMemo(() => {
    if (!flatWords.length || !time) return null;
    let lo = 0;
    let hi = flatWords.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const w = flatWords[mid];
      if (time < w.start) hi = mid - 1;
      else if (time > w.end) lo = mid + 1;
      else return w.key;
    }
    // Between words (a pause) — hold the last one that already started.
    return hi >= 0 ? flatWords[hi].key : null;
  }, [flatWords, time]);

  // Edited paragraphs have no usable word timings, so they follow along at
  // paragraph granularity instead of losing the highlight entirely.
  const activeParagraph = useMemo(() => {
    if (!time) return null;
    const i = paragraphs.findIndex((p) => time >= p.start && time <= p.end);
    return i === -1 ? null : i;
  }, [paragraphs, time]);

  // Keep the spoken word on screen, but only nudge when it drifts outside a
  // comfortable band — scrolling on every single word would be unreadable.
  useEffect(() => {
    if (editing !== null || tweening.current) return;
    const el = activeWordRef.current;
    const container = scrollRef.current;
    if (!el || !container) return;
    const cRect = container.getBoundingClientRect();
    const eRect = el.getBoundingClientRect();

    // The lower bound is the top of the player, not the bottom of the window —
    // otherwise the spoken word "stays visible" by sliding under the transport.
    const top = cRect.top + cRect.height * 0.25;
    const bottom = cRect.bottom - PLAYER_SAFE_PX;
    if (eRect.top < top || eRect.bottom > bottom) {
      container.scrollTo({
        top:
          container.scrollTop +
          (eRect.top - (cRect.top + cRect.height * 0.42)),
        behavior: "smooth",
      });
    }
  }, [activeKey, editing]);

  // Which paragraph is being read. Playback wins while the spoken paragraph is
  // on screen; otherwise the one crossing the reading line, so scrolling ahead
  // while paused moves focus with the eye rather than leaving it behind.
  const pickFocus = useCallback(() => {
    const container = scrollRef.current;
    // The focus tween drives scrollTop itself, which fires scroll events.
    // Re-deriving focus from those would let the tween retarget itself
    // mid-flight and oscillate.
    if (!container || tweening.current) return;
    const cRect = container.getBoundingClientRect();

    if (activeParagraph !== null) {
      const el = paraRefs.current[activeParagraph];
      const r = el?.getBoundingClientRect();
      if (r && r.bottom > cRect.top && r.top < cRect.bottom) {
        setFocus(activeParagraph);
        return;
      }
    }

    const line = cRect.top + cRect.height * 0.38;
    let best = 0;
    let bestDist = Infinity;
    paraRefs.current.forEach((el, i) => {
      if (!el) return;
      const r = el.getBoundingClientRect();
      const d = r.top > line ? r.top - line : Math.max(0, line - r.bottom);
      if (d < bestDist) {
        bestDist = d;
        best = i;
      }
    });
    setFocus(best);
  }, [activeParagraph]);

  useEffect(() => {
    pickFocus();
  }, [pickFocus, paragraphs]);

  // Scroll fires far faster than paint, and each pass measures every paragraph.
  // Collapsing to one measurement per frame keeps a long transcript from
  // spending its whole frame budget in layout.
  const scrollRaf = useRef(0);
  const onScroll = useCallback(() => {
    if (scrollRaf.current) return;
    scrollRaf.current = requestAnimationFrame(() => {
      scrollRaf.current = 0;
      pickFocus();
    });
  }, [pickFocus]);

  useEffect(
    () => () => {
      if (scrollRaf.current) cancelAnimationFrame(scrollRaf.current);
    },
    [],
  );

  // Animate the focus shift. Only two paragraphs actually change, so tween
  // those rather than the whole document — on a 126-paragraph transcript a
  // blanket tween would restyle every node on every word boundary.
  const prevFocus = useRef(-1);
  // Which transcript the current inline styles belong to. Without this, opening
  // a second transcript reuses the same <p> nodes but only re-styles the two
  // paragraphs involved in the focus change, leaving the rest at the previous
  // document's sizes — or at the 13px body default.
  const styledFor = useRef<string | null>(null);

  useLayoutEffect(() => {
    const container = scrollRef.current;
    if (!container) return;

    const apply = (i: number, focused: boolean, animate: boolean) => {
      const el = paraRefs.current[i];
      if (!el) return;
      const to = {
        fontSize: focused ? FOCUS_SIZE : REST_SIZE,
        opacity: focused ? 1 : REST_OPACITY,
      };
      if (!animate) gsap.set(el, to);
      else gsap.to(el, { ...to, duration: 0.38, ease: EASE.snap });
    };

    const fresh = styledFor.current !== transcript.id;
    if (fresh || prefersReducedMotion()) {
      // Drive from the data, not the ref array: a shorter transcript would
      // otherwise leave the previous one's trailing refs in play.
      paraRefs.current.length = paragraphs.length;
      paragraphs.forEach((_, i) => apply(i, i === focus, false));
      styledFor.current = transcript.id;
      prevFocus.current = focus;
      return;
    }
    if (prevFocus.current === focus) return;

    // Growing a paragraph reflows everything below it, so the tween also owns
    // scroll: it drives the focused paragraph's top from where it is to where
    // it should be. When it's already comfortably placed that target is its
    // current position, which reduces to holding it still while it resizes.
    const el = paraRefs.current[focus];
    tweening.current = true;

    const cRect = container.getBoundingClientRect();
    const rect = el?.getBoundingClientRect();
    const safeBottom = cRect.bottom - PLAYER_SAFE_PX;
    const startTop = rect ? rect.top : 0;

    let targetTop = startTop;
    if (rect) {
      const settled = rect.top >= cRect.top && rect.bottom <= safeBottom;
      // Growing shifts the bottom edge down, so judge against the post-tween
      // height — otherwise a paragraph that just fits gets clipped by the
      // player the moment it enlarges.
      const grown = rect.height * (FOCUS_SIZE / REST_SIZE);
      const willOverflow = rect.top + grown > safeBottom;
      if (!settled || willOverflow) {
        targetTop = cRect.top + cRect.height * READING_LINE;
      }
    }

    apply(prevFocus.current, false, true);
    apply(focus, true, true);

    const state = { t: 0 };
    gsap.to(state, {
      t: 1,
      duration: 0.38,
      ease: EASE.snap,
      onUpdate: () => {
        if (!el) return;
        const want = startTop + (targetTop - startTop) * state.t;
        container.scrollTop += el.getBoundingClientRect().top - want;
      },
      onComplete: () => {
        if (el) {
          container.scrollTop += el.getBoundingClientRect().top - targetTop;
        }
        tweening.current = false;
      },
    });

    prevFocus.current = focus;
  }, [focus, paragraphs, transcript.id]);

  // Drop the caret where the click landed. The plain text renders at the same
  // metrics as the spans it replaced, so the same point maps to the same
  // character — no offset bookkeeping needed.
  useLayoutEffect(() => {
    if (editing === null) return;
    const el = editRef.current;
    const point = caretPoint.current;
    el?.focus();
    if (!el || !point) return;
    const range = document.caretRangeFromPoint?.(point.x, point.y);
    if (range && el.contains(range.startContainer)) {
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range);
    }
    caretPoint.current = null;
  }, [editing]);

  const onTime = useCallback((t: number) => setTime(t), []);

  const enterEdit = (pi: number, e: React.MouseEvent) => {
    caretPoint.current = { x: e.clientX, y: e.clientY };
    if (flashTimer.current !== null) clearTimeout(flashTimer.current);
    // Let the highlight register before the caret takes over, so a click reads
    // as "you are here" first and "you are typing" second.
    flashTimer.current = window.setTimeout(() => setEditing(pi), FLASH_MS);
  };

  const clickWord = (pi: number, wi: number, start: number, e: React.MouseEvent) => {
    setFlash(`${pi}:${wi}`);
    playerRef.current?.seek(start, { pauseFirst: true });
    enterEdit(pi, e);
  };

  const commitEdit = (pi: number) => {
    const el = editRef.current;
    setEditing(null);
    setFlash(null);
    if (!el) return;

    const next = el.innerText.replace(/\s+/g, " ").trim();
    const para = paragraphs[pi];
    if (!next || next === para.text) return;

    // Re-fit the surviving word timings onto the new text instead of discarding
    // them, so fixing one word costs follow-along for that word alone.
    const words = para.words?.length ? reconcileWords(para.words, next) : [];
    const updated = paragraphs.map((p, i) =>
      i === pi ? { ...p, text: next, words, edited: true } : p,
    );
    setParagraphs(updated);
    onEdit(transcript.id, updated);
  };

  const commitTitle = () => {
    const next = title.trim();
    if (!next || next === transcript.title) {
      setTitle(transcript.title);
      return;
    }
    onRename(transcript.id, next);
  };

  const fullText = () => paragraphs.map((p) => p.text).join("\n\n");

  const copyAll = async () => {
    await navigator.clipboard.writeText(fullText());
    setCopied(true);
  };

  const wordCount = paragraphs.reduce(
    (n, p) => n + p.text.split(/\s+/).filter(Boolean).length,
    0,
  );

  /** The line under the title, in both the PDF and the Markdown front matter. */
  const dateline = () =>
    [
      formatDuration(transcript.duration),
      `${wordCount.toLocaleString()} words`,
      transcript.language?.toUpperCase(),
      new Date(transcript.created_at).toLocaleDateString(undefined, {
        day: "numeric",
        month: "short",
        year: "numeric",
      }),
    ]
      .filter(Boolean)
      .join(" · ");

  const exportFile = async () => {
    const target = await save({
      defaultPath: `${transcript.title}.pdf`,
      filters: [
        { name: "PDF", extensions: ["pdf"] },
        { name: "Markdown", extensions: ["md"] },
        { name: "Plain text", extensions: ["txt"] },
      ],
    });
    if (!target) return;

    setExporting(true);
    setExportError(null);
    try {
      if (target.endsWith(".pdf")) {
        await exportPdf(target, {
          title: transcript.title,
          meta: dateline(),
          // Timestamps are hung in the margin of the PDF, so every paragraph
          // carries the one from the audio it starts at.
          paragraphs: paragraphs.map((p) => ({
            stamp: formatDuration(p.start),
            text: p.text,
          })),
        });
        return;
      }
      const body = target.endsWith(".txt")
        ? fullText()
        : [`# ${transcript.title}`, "", `*${dateline()}*`, "", fullText()].join(
            "\n",
          );
      await writeTextFile(target, body);
    } catch (err) {
      // Saving is the one place a silent failure is unforgivable: the user
      // walks away believing the file exists.
      setExportError(String(err));
    } finally {
      setExporting(false);
    }
  };

  return (
    // No dot grid, no swarm here. Texture behind running text is exactly what
    // hurts readability, and §1 is explicit that it never sits under content.
    <div className="relative h-full bg-surface">
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="scroll-slim h-full overflow-y-auto"
      >
        <header className="drag-region titlebar-pad sticky top-0 z-10 border-b border-hairline bg-surface/95 backdrop-blur-xl">
          <div className="mx-auto flex max-w-[680px] items-start gap-4 px-8 pb-3">
            <div className="min-w-0 flex-1">
              <input
                key={titleRenamed ? `named:${transcript.title}` : "title"}
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                onBlur={commitTitle}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                  if (e.key === "Escape") {
                    setTitle(transcript.title);
                    e.currentTarget.blur();
                  }
                }}
                className={[
                  titleRenamed ? "title-reveal" : "",
                  "selectable no-drag w-full truncate border border-transparent bg-transparent text-[19px] font-medium leading-tight tracking-[-0.01em] text-ink outline-none hover:border-hairline focus:border-sage-dim",
                ].join(" ")}
              />
              <p className="mono-data mt-1 flex items-center gap-1.5 text-[10px] uppercase tracking-[0.12em] text-faint">
                {naming ? (
                  <span className="flex items-center gap-1.5 text-sage-dim">
                    <span className="animate-pulse">
                      <PixelCluster pattern={CLUSTERS.brand} size={2.5} pulse />
                    </span>
                    NAMING VIA AI…
                  </span>
                ) : (
                  <>
                    {formatDuration(transcript.duration)} //{" "}
                    {wordCount.toLocaleString()} WORDS
                    {transcript.language
                      ? ` // ${transcript.language.toUpperCase()}`
                      : ""}
                  </>
                )}
              </p>
              {exportError && (
                // A save that quietly did nothing is the worst outcome here —
                // the user closes the window believing the file is on disk.
                <p className="mono-data mt-1 text-[10px] uppercase tracking-[0.12em] text-amber">
                  EXPORT FAILED — {exportError}
                </p>
              )}
            </div>

            <div className="no-drag flex shrink-0 items-center gap-px pt-1">
              <RailButton onClick={copyAll} label={copied ? "COPIED" : "COPY"} />
              <RailButton
                onClick={exportFile}
                label={exporting ? "SAVING…" : "EXPORT"}
              />
              <RailButton
                onClick={() =>
                  // Prefer where the file came from; fall back to the archived
                  // copy for anything that arrived without an original on disk.
                  revealItemInDir(
                    transcript.origin_path || transcript.source_path,
                  ).catch(() => {})
                }
                label="SOURCE"
              />
              <RailButton
                onClick={() => onDelete(transcript.id)}
                label="DELETE"
                danger
              />
            </div>
          </div>
        </header>

        {/* Bottom padding clears the floating player, which overlays the text. */}
        <article className="mx-auto max-w-[680px] px-8 pb-32 pt-9">
          {/* ~1.8 leading on a ~66-character measure. Font size and opacity are
              owned by GSAP, not React — see the focus effect above; declaring
              them here too would let a re-render stomp mid-tween. */}
          <div className="space-y-[1.4em] leading-[1.75] text-ink">
            {paragraphs.map((p, pi) =>
              editing === pi ? (
                <p
                  key={pi}
                  ref={(el) => {
                    editRef.current = el;
                    paraRefs.current[pi] = el;
                  }}
                  contentEditable
                  suppressContentEditableWarning
                  // Uncontrolled while focused: React must not re-render this
                  // subtree mid-keystroke or the caret jumps to the start.
                  onBeforeInput={() => playerRef.current?.cancelResume()}
                  onBlur={() => commitEdit(pi)}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") {
                      e.preventDefault();
                      setEditing(null);
                    }
                  }}
                  className="selectable -mx-2 border-l-2 border-sage bg-hairline-soft px-2 outline-none"
                >
                  {p.text}
                </p>
              ) : (
                <p
                  key={pi}
                  ref={(el) => {
                    paraRefs.current[pi] = el;
                  }}
                  onClick={(e) => {
                    if (!p.words?.length) {
                      playerRef.current?.seek(p.start, { pauseFirst: true });
                      enterEdit(pi, e);
                    }
                  }}
                  className={[
                    "selectable -mx-3 cursor-text border-l-2 px-3 transition-[border-color]",
                    focus === pi ? "border-sage-dim" : "border-transparent",
                  ].join(" ")}
                >
                  {p.words?.length
                    ? p.words.map((w, wi) => {
                        const key = `${pi}:${wi}`;
                        const lit = key === activeKey || key === flash;
                        return (
                          <span
                            key={wi}
                            ref={key === activeKey ? activeWordRef : undefined}
                            onClick={(e) => clickWord(pi, wi, w.start, e)}
                            className={[
                              lit ? "bg-sage text-forest" : "transition-colors duration-150",
                              w.edited && !lit ? "word-edited" : "",
                            ].join(" ")}
                          >
                            {w.text}{" "}
                          </span>
                        );
                      })
                    : p.text}
                </p>
              ),
            )}
          </div>

          {!hasWords && (
            <p className="mono-data mt-8 border-t border-hairline pt-3 text-[10px] uppercase tracking-[0.12em] text-faint">
              NO WORD TIMINGS // FOLLOW-ALONG RUNS BY PARAGRAPH
            </p>
          )}

          <footer className="mt-10 border-t border-hairline pt-3">
            <span className="diagnostic">{fileName(transcript.source_path)}</span>
          </footer>
        </article>
      </div>

      <AudioPlayer
        key={mediaPath}
        sourcePath={mediaPath}
        duration={transcript.duration}
        peaks={peaks}
        onTime={onTime}
        handleRef={playerRef}
      />
    </div>
  );
}

function RailButton({
  label,
  onClick,
  danger,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={[
        "micro border border-hairline px-2.5 py-1.5 transition-colors",
        danger
          ? "text-faint hover:border-amber hover:bg-amber hover:text-surface"
          : "text-grey hover:border-ink hover:bg-ink hover:text-surface",
      ].join(" ")}
    >
      {label}
    </button>
  );
}
