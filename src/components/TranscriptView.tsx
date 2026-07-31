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

/** Type scale for the focused paragraph vs the rest, *while following audio*. */
const FOCUS_SIZE = 19.5;
const REST_SIZE = 17;
const REST_OPACITY = 0.45;

/**
 * One size, full contrast: how the document looks with the player idle.
 *
 * Dimming everything you are not looking at is right when the audio decides
 * where that is. With playback stopped nothing does, so the same treatment just
 * greys out three-quarters of the page and makes the reader chase a highlight
 * that isn't tracking anything. Reading and following are different jobs.
 */
const READ_SIZE = 18;

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
  // Following = audio is running. Everything about how the page looks and
  // whether it scrolls itself hangs off this one flag.
  const [following, setFollowing] = useState(false);
  const [finding, setFinding] = useState(false);
  const [query, setQuery] = useState("");
  const [matchAt, setMatchAt] = useState(0);

  const scrollRef = useRef<HTMLDivElement>(null);
  const playerRef = useRef<PlayerHandle | null>(null);
  const activeWordRef = useRef<HTMLSpanElement | null>(null);
  const findRef = useRef<HTMLInputElement | null>(null);
  const currentMatchRef = useRef<HTMLElement | null>(null);
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
    setFinding(false);
    setQuery("");
    setMatchAt(0);
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

  // -- find in transcript ---------------------------------------------------

  /**
   * What each paragraph *renders as*, which is not always `p.text`.
   *
   * A paragraph with word timings is drawn as one span per word joined by
   * single spaces; an edited one is drawn as its raw text. Searching the same
   * string the reader is looking at is what keeps a match's character offsets
   * mapping onto the right words — deriving them from `p.text` instead would
   * drift the moment an edit reconciled the word list differently.
   */
  const rendered = useMemo(
    () =>
      paragraphs.map((p) =>
        p.words?.length ? p.words.map((w) => w.text).join(" ") : p.text,
      ),
    [paragraphs],
  );

  /** Every hit, in reading order, as a paragraph index and character range. */
  const matches = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q.length < 2) return [] as { pi: number; from: number; to: number }[];
    const found: { pi: number; from: number; to: number }[] = [];
    rendered.forEach((text, pi) => {
      const hay = text.toLowerCase();
      let at = hay.indexOf(q);
      while (at !== -1) {
        found.push({ pi, from: at, to: at + q.length });
        at = hay.indexOf(q, at + q.length);
      }
    });
    return found;
  }, [rendered, query]);

  // A new query invalidates the cursor; clamping rather than resetting keeps
  // your place when you only refine the tail of a word.
  useEffect(() => {
    setMatchAt((i) => (matches.length ? Math.min(i, matches.length - 1) : 0));
  }, [matches.length]);

  /**
   * Word indices to highlight per paragraph, keyed `pi:wi`, plus the subset
   * belonging to the current hit.
   *
   * Word spans can't be split mid-word, so a match lights every word it touches
   * — searching "motion" in "emotional" lights that whole word. Honest about
   * where the hit is, and no worse than what a browser's own find does to a
   * ligature.
   */
  const marked = useMemo(() => {
    const all = new Set<string>();
    const current = new Set<string>();
    if (!matches.length) return { all, current };

    // Cumulative offset of each word in the joined string, computed once per
    // paragraph that actually has a hit rather than for the whole document.
    const spans = new Map<number, { from: number; to: number }[]>();
    const spansFor = (pi: number) => {
      let s = spans.get(pi);
      if (!s) {
        s = [];
        let at = 0;
        for (const w of paragraphs[pi]?.words ?? []) {
          s.push({ from: at, to: at + w.text.length });
          at += w.text.length + 1; // the joining space
        }
        spans.set(pi, s);
      }
      return s;
    };

    matches.forEach((m, mi) => {
      const words = paragraphs[m.pi]?.words;
      const into = mi === matchAt ? [all, current] : [all];
      if (!words?.length) {
        // Plain-text paragraph: the whole paragraph carries the highlight,
        // since there are no per-word nodes to attach it to.
        into.forEach((set) => set.add(`${m.pi}:text`));
        return;
      }
      spansFor(m.pi).forEach((s, wi) => {
        if (s.from < m.to && s.to > m.from) {
          into.forEach((set) => set.add(`${m.pi}:${wi}`));
        }
      });
    });
    return { all, current };
  }, [matches, matchAt, paragraphs]);

  const stepMatch = useCallback(
    (delta: number) => {
      if (!matches.length) return;
      setMatchAt((i) => (i + delta + matches.length) % matches.length);
    },
    [matches.length],
  );

  // Bring the current hit into view. Plain scrollIntoView is right here: unlike
  // the follow-along scroll there's no tween competing for scrollTop.
  useEffect(() => {
    if (!finding || !matches.length) return;
    currentMatchRef.current?.scrollIntoView({
      block: "center",
      behavior: "smooth",
    });
  }, [finding, matchAt, matches.length]);

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

  const prevFollowing = useRef(false);

  useLayoutEffect(() => {
    const container = scrollRef.current;
    if (!container) return;

    // Reading is one size at full contrast; following is the focused paragraph
    // enlarged with the rest pulled back.
    const styleFor = (i: number) =>
      !following
        ? { fontSize: READ_SIZE, opacity: 1 }
        : {
            fontSize: i === focus ? FOCUS_SIZE : REST_SIZE,
            opacity: i === focus ? 1 : REST_OPACITY,
          };

    const apply = (i: number, animate: boolean) => {
      const el = paraRefs.current[i];
      if (!el) return;
      const to = styleFor(i);
      if (!animate) gsap.set(el, to);
      else gsap.to(el, { ...to, duration: 0.38, ease: EASE.snap });
    };

    const fresh = styledFor.current !== transcript.id;
    if (fresh || prefersReducedMotion()) {
      // Drive from the data, not the ref array: a shorter transcript would
      // otherwise leave the previous one's trailing refs in play.
      paraRefs.current.length = paragraphs.length;
      paragraphs.forEach((_, i) => apply(i, false));
      styledFor.current = transcript.id;
      prevFocus.current = focus;
      prevFollowing.current = following;
      return;
    }

    // Switching modes restyles the whole document at once, which is the one
    // time a blanket tween is justified — it happens on a play/pause, not on
    // every word boundary.
    if (prevFollowing.current !== following) {
      prevFollowing.current = following;
      prevFocus.current = focus;

      const cRect = container.getBoundingClientRect();
      const anchor = paraRefs.current[focus];
      const holdAt = anchor?.getBoundingClientRect().top ?? null;

      paragraphs.forEach((_, i) => {
        const el = paraRefs.current[i];
        if (!el) return;
        // Offscreen paragraphs get the new size instantly. Nobody can see them
        // animate, and tweening several hundred nodes at once would cost a
        // frame budget that the visible ones need.
        const r = el.getBoundingClientRect();
        apply(i, r.bottom > cRect.top && r.top < cRect.bottom);
      });

      // Every line in the document just changed size, so the text under the
      // reader's eye would otherwise slide. Pin the paragraph they were on.
      if (anchor && holdAt !== null) {
        const state = { t: 0 };
        gsap.to(state, {
          t: 1,
          duration: 0.38,
          ease: EASE.snap,
          onUpdate: () => {
            container.scrollTop += anchor.getBoundingClientRect().top - holdAt;
          },
        });
      }
      return;
    }

    // While reading, focus tracks the scroll position for the margin marker
    // only — it must not resize anything.
    if (!following) {
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

    apply(prevFocus.current, true);
    apply(focus, true);

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
  }, [focus, paragraphs, transcript.id, following]);

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

  /** Put a paragraph on the reading line and make it the focused one. */
  const goToParagraph = useCallback((i: number) => {
    const container = scrollRef.current;
    const el = paraRefs.current[i];
    if (!container || !el) return;
    const cRect = container.getBoundingClientRect();
    container.scrollTo({
      top:
        container.scrollTop +
        (el.getBoundingClientRect().top -
          (cRect.top + cRect.height * READING_LINE)),
      behavior: "smooth",
    });
    setFocus(i);
  }, []);

  // Reading keys. Deliberately not registered while the caret is in prose or a
  // field — ArrowUp in a paragraph you're editing must move the caret, and F
  // must type an f.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      const typing =
        !!el &&
        (["INPUT", "TEXTAREA"].includes(el.tagName) || el.isContentEditable);

      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setFinding(true);
        // Already open: re-select, so a second ⌘F replaces the query rather
        // than appending to it — the behaviour every other find bar has.
        findRef.current?.select();
        findRef.current?.focus();
        return;
      }

      if (e.key === "Escape" && finding) {
        e.preventDefault();
        setFinding(false);
        setQuery("");
        return;
      }

      if (typing) return;

      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const next = focus + (e.key === "ArrowDown" ? 1 : -1);
        if (next >= 0 && next < paragraphs.length) goToParagraph(next);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [finding, focus, paragraphs.length, goToParagraph]);

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
    <div className="reading-column relative h-full bg-surface">
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="scroll-slim h-full overflow-y-auto"
      >
        <header className="drag-region titlebar-pad sticky top-0 z-10 border-b border-hairline bg-surface/95 backdrop-blur-xl">
          {/* Same column as the prose, so the title sits on the reading line
              and the timestamp gutter runs clear of both. */}
          <div className="reading-body flex items-start gap-4 pb-3">
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

        {/* The gutter is 40px of stamp and 14px of gap, matching the PDF
            exactly, so a printed transcript and the one on screen have the same
            anatomy. Bottom padding clears the floating player, which overlays
            the text. */}
        <article className="reading-body pb-32 pt-9">
          {/* ~1.75 leading on a ~66-character measure. Font size and opacity are
              owned by GSAP, not React — see the focus effect above; declaring
              them here too would let a re-render stomp mid-tween. */}
          <div className="space-y-[1.4em] leading-[1.75] text-ink">
            {paragraphs.map((p, pi) => (
              <div key={pi} className="relative">
                {/* Hung in the margin, never in the reading line. It answers
                    "how far in is this?" without ever interrupting a sentence,
                    and seeks when clicked. */}
                <button
                  onClick={() =>
                    playerRef.current?.seek(p.start, { pauseFirst: true })
                  }
                  tabIndex={-1}
                  aria-label={`Play from ${formatDuration(p.start)}`}
                  className="para-stamp mono-data no-drag text-left text-[10px] tabular-nums tracking-[0.06em] text-faint opacity-60 transition-opacity hover:text-ink hover:opacity-100"
                >
                  {formatDuration(p.start)}
                </button>

                {editing === pi ? (
                  <p
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
                    ref={(el) => {
                      paraRefs.current[pi] = el;
                      // A paragraph with no word timings carries the highlight
                      // whole, so it is also the scroll target for its own hit.
                      if (marked.current.has(`${pi}:text`)) {
                        currentMatchRef.current = el;
                      }
                    }}
                    onClick={(e) => {
                      if (!p.words?.length) {
                        playerRef.current?.seek(p.start, { pauseFirst: true });
                        enterEdit(pi, e);
                      }
                    }}
                    className={[
                      "selectable -mx-3 cursor-text border-l-2 px-3 transition-[border-color]",
                      // The marker tracks the spoken paragraph. While reading,
                      // nothing is speaking, so there is nothing to mark.
                      following && focus === pi
                        ? "border-sage-dim"
                        : "border-transparent",
                      marked.all.has(`${pi}:text`) ? "find-hit" : "",
                    ].join(" ")}
                  >
                    {p.words?.length
                      ? p.words.map((w, wi) => {
                          const key = `${pi}:${wi}`;
                          const lit = key === activeKey || key === flash;
                          const hit = marked.all.has(key);
                          const here = marked.current.has(key);
                          return (
                            <span
                              key={wi}
                              // Only the two spans that need tracking carry a
                              // ref. A callback on every word would have React
                              // detaching and reattaching thousands of refs on
                              // each playback frame, since the closure is new
                              // every render.
                              ref={
                                key === activeKey || here
                                  ? (el) => {
                                      if (key === activeKey) {
                                        activeWordRef.current = el;
                                      }
                                      if (here) currentMatchRef.current = el;
                                    }
                                  : undefined
                              }
                              onClick={(e) => clickWord(pi, wi, w.start, e)}
                              className={[
                                lit
                                  ? "bg-sage text-forest"
                                  : "transition-colors duration-150",
                                !lit && here
                                  ? "find-hit-current"
                                  : !lit && hit
                                    ? "find-hit"
                                    : "",
                                w.edited && !lit && !hit ? "word-edited" : "",
                              ].join(" ")}
                            >
                              {w.text}{" "}
                            </span>
                          );
                        })
                      : p.text}
                  </p>
                )}
              </div>
            ))}
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

      {finding && (
        // Pinned to the reading pane rather than the window, for the same
        // reason the transport is: fixed positioning centres on the window and
        // drifts left of the text once the sidebar takes its width.
        <div className="absolute right-6 top-[46px] z-20 flex items-center gap-2 border border-hairline bg-panel px-2.5 py-1.5 shadow-sm">
          <input
            ref={findRef}
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                stepMatch(e.shiftKey ? -1 : 1);
              }
            }}
            placeholder="FIND"
            className="mono-data w-[150px] bg-transparent text-[11px] uppercase tracking-[0.1em] text-ink outline-none placeholder:text-faint"
          />
          <span className="mono-data w-[52px] shrink-0 text-right text-[10px] tabular-nums tracking-[0.08em] text-faint">
            {query.trim().length < 2
              ? ""
              : matches.length
                ? `${matchAt + 1}/${matches.length}`
                : "NONE"}
          </span>
          <button
            onClick={() => stepMatch(-1)}
            aria-label="Previous match"
            className="micro border border-hairline px-1.5 py-0.5 text-grey transition-colors hover:border-ink hover:bg-ink hover:text-surface"
          >
            ‹
          </button>
          <button
            onClick={() => stepMatch(1)}
            aria-label="Next match"
            className="micro border border-hairline px-1.5 py-0.5 text-grey transition-colors hover:border-ink hover:bg-ink hover:text-surface"
          >
            ›
          </button>
          <button
            onClick={() => {
              setFinding(false);
              setQuery("");
            }}
            aria-label="Close find"
            className="micro border border-hairline px-1.5 py-0.5 text-faint transition-colors hover:border-ink hover:bg-ink hover:text-surface"
          >
            ✕
          </button>
        </div>
      )}

      <AudioPlayer
        key={mediaPath}
        sourcePath={mediaPath}
        duration={transcript.duration}
        peaks={peaks}
        onTime={onTime}
        onPlayingChange={setFollowing}
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
