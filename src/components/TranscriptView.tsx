import {
  Fragment,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import gsap from "gsap";

import type {
  Brief,
  BriefCapability,
  BriefProgress,
  Paragraph,
  Transcript,
} from "../lib/api";
import {
  archiveTranscriptMedia,
  briefCapability,
  exportPdf,
  fetchPeaks,
  generateBrief,
  namesInMeeting,
  setTranscriptPeaks,
  watchBriefFailed,
  watchBriefProgress,
  watchBriefSaved,
  writeTextFile,
} from "../lib/api";
import { reconcileWords } from "../lib/diff";
import { fileName, formatDuration, formatWhen } from "../lib/format";
import { EASE, prefersReducedMotion } from "../lib/motion";
import { AudioPlayer, type PlayerHandle } from "./AudioPlayer";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

type Props = {
  transcript: Transcript;
  onRename: (id: string, title: string) => void;
  onDelete: (id: string) => void;
  onEdit: (id: string, paragraphs: Paragraph[]) => void;
  /** Name one side of a meeting. Rejects with a sentence worth showing. */
  onRenameSpeaker: (id: string, from: string, to: string) => Promise<void>;
  /** The AI is currently generating this note's title. */
  naming?: boolean;
};

/** How long the clicked word stays lit before the caret takes over. */
const FLASH_MS = 420;

/**
 * Containers WKWebView will actually decode. Mirrors `media::is_playable` — the
 * backend decides whether to re-encode, this decides whether to bother asking.
 */
const PLAYABLE = /\.(m4a|mp4|mp3|wav|aac|aiff|aif|caf|flac)$/i;
const playableAudio = (path: string) => PLAYABLE.test(path);

type Placed = { prefix: string; text: string; from: number; to: number };

/**
 * Lay a paragraph's words back onto its own text.
 *
 * The two are not in the same shape, and neither one can be spaced by rule:
 *
 * - Fresh from whisper, `words` are *BPE tokens*, not words. "graphify" arrives
 *   as `" graph"` + `"ify"`, and each token carries the whitespace that belongs
 *   in front of it — punctuation like `","` carries none.
 * - After an edit, `reconcileWords` splits the typed text on whitespace, so its
 *   tokens are bare words with no spacing encoded at all.
 *
 * Joining with a space is right for the second and wrong for the first, which
 * is what put a gap in the middle of "graph ify" and in front of every comma.
 * Concatenating raw is right for the first and wrong for the second. There is no
 * per-token test that separates them.
 *
 * So neither is used as the source of spacing: `text` is. Each token is found in
 * the paragraph's own text in order, and whatever sits between one token and the
 * next is emitted verbatim. The rendered characters are then `text`, exactly —
 * which is what COPY has always used, and why that button was already correct
 * while selecting the same words with a cursor was not.
 *
 * The offsets come back too, so search matches land on the right words instead
 * of being hunted for in a string nothing renders.
 */
function placeWords(text: string, words: { text: string }[]): Placed[] {
  const out: Placed[] = [];
  let at = 0;
  for (const w of words) {
    const token = w.text.trim();
    // One entry per input word, always: `flatWords` and the highlight sets key
    // words by their index in `p.words`, and a skipped token here would shift
    // every following word onto the wrong timing.
    if (!token) {
      out.push({ prefix: "", text: "", from: at, to: at });
      continue;
    }
    const found = text.indexOf(token, at);
    if (found === -1) {
      // Timings that no longer describe this text — a reconcile that dropped
      // out of step. Keep the word clickable and readable rather than losing it.
      out.push({
        prefix: out.length ? " " : "",
        text: token,
        from: at,
        to: at + token.length,
      });
      continue;
    }
    out.push({
      prefix: text.slice(at, found),
      text: token,
      from: found,
      to: found + token.length,
    });
    at = found + token.length;
  }
  return out;
}

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

export function TranscriptView({
  transcript,
  onRename,
  onDelete,
  onEdit,
  onRenameSpeaker,
  naming = false,
}: Props) {
  const [copied, setCopied] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const [title, setTitle] = useState(transcript.title);
  const [time, setTime] = useState(0);
  const [paragraphs, setParagraphs] = useState<Paragraph[]>([]);
  const [editing, setEditing] = useState<number | null>(null);
  /** Which turn's speaker label is open for editing, and what has been typed
   *  into it. Keyed on the paragraph rather than the name so only the label you
   *  clicked turns into a field — the rename still lands on every turn by that
   *  speaker, but a meeting where six labels became inputs at once would look
   *  like six separate edits. */
  const [namingSpeakerAt, setNamingSpeakerAt] = useState<number | null>(null);
  const [speakerDraft, setSpeakerDraft] = useState("");
  const [speakerError, setSpeakerError] = useState<string | null>(null);
  /** Names the model heard in this call, once asked. Null until then — which is
   *  what makes the ask happen exactly once per note. */
  const [heardNames, setHeardNames] = useState<string[] | null>(null);
  const [listening, setListening] = useState(false);
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
  // The overview half of the note. Held locally as well as on the prop so a
  // freshly generated brief appears without waiting for the parent to refetch.
  const [brief, setBrief] = useState<Brief | null>(transcript.brief);
  const [pane, setPane] = useState<"overview" | "transcript">("transcript");
  const [briefing, setBriefing] = useState(false);
  const [briefError, setBriefError] = useState<string | null>(null);
  // Null until the probe answers, so the toggle doesn't flicker into view and
  // out again on a Mac that cannot make overviews.
  const [canBrief, setCanBrief] = useState<BriefCapability | null>(null);
  // Which pass the on-device model is on. A long meeting is read in pieces and
  // the whole thing can take a couple of minutes; a label that never changes
  // for that long is indistinguishable from one that has hung.
  const [briefStage, setBriefStage] = useState<BriefProgress | null>(null);

  useEffect(() => {
    briefCapability().then(setCanBrief, () =>
      // An unregistered command means a build with no overviews at all.
      setCanBrief({
        available: false,
        reason: "helper-missing",
        on_device: false,
        message: "",
      }),
    );
  }, []);

  // Apple Intelligence reports "still downloading" but never how far through,
  // and nothing tells us when it lands — the framework offers a state and no
  // progress and no notification. So rather than asking someone to keep coming
  // back and pressing a dead button, we come back for them. Only while the pane
  // is actually showing that state, and every few seconds rather than
  // constantly: each check is a process spawn, cheap but not free.
  useEffect(() => {
    if (canBrief?.reason !== "model-not-ready") return;
    const timer = setInterval(() => {
      briefCapability().then(setCanBrief, () => {});
    }, 8000);
    return () => clearInterval(timer);
  }, [canBrief?.reason]);

  // Always listening, not only while this window started something. A meeting
  // briefs itself as it saves, so the first time a reader sees this pane the
  // work may already be under way — and the note they are looking at may not be
  // the note being read, which is why every event carries an id.
  useEffect(() => {
    const id = transcript.id;
    const subs = [
      watchBriefProgress((p) => {
        if (p.id !== id) return;
        setBriefStage(p);
        setBriefing(p.progress < 1);
      }),
      watchBriefSaved((p) => {
        if (p.id !== id) return;
        setBrief(p.brief);
        setBriefing(false);
        setBriefStage(null);
        setBriefError(null);
        // Show it. The overview is now written unasked, so this is the moment
        // it becomes worth reading — for a meeting you have just left or a file
        // you have just dropped in, it is the answer to why you opened the app.
        //
        // Only if the reader has not gone looking for something in the
        // transcript. Someone mid-scrub or mid-search is doing something the
        // summary does not answer, and pulling the page out from under them
        // would be the app deciding it knows better.
        setPane((current) =>
          current === "transcript" && !readerBusy.current ? "overview" : current,
        );
      }),
      watchBriefFailed((p) => {
        if (p.id !== id) return;
        setBriefing(false);
        setBriefStage(null);
        setBriefError(p.problem);
      }),
    ];
    return () => {
      subs.forEach((s) => s.then((un) => un()).catch(() => {}));
    };
  }, [transcript.id]);

  /** Whether the reader is in the middle of something the summary doesn't
   *  answer. Held in a ref because the brief handler above subscribes once per
   *  note: reading these off the closure would give it whatever they were when
   *  the note opened, which is always "no". */
  const readerBusy = useRef(false);
  useEffect(() => {
    readerBusy.current = finding || following;
  }, [finding, following]);

  // Who was heard in *this* call. Cleared on the way to another note, or the
  // second meeting you opened would be offered the first one's names.
  useEffect(() => {
    setHeardNames(null);
    setListening(false);
    setNamingSpeakerAt(null);
    setSpeakerError(null);
  }, [transcript.id]);

  /** Heard names that aren't already labelling somebody here. The backend
   *  filters this too, but its answer was computed before the rename that is
   *  most likely to make it stale — the one just made from this very list. */
  const suggestions = useMemo(() => {
    if (!heardNames) return [];
    const taken = new Set(
      paragraphs
        .map((p) => p.speaker?.toLowerCase())
        .filter((s): s is string => !!s),
    );
    return heardNames.filter((name) => !taken.has(name.toLowerCase()));
  }, [heardNames, paragraphs]);

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
    setBrief(transcript.brief);
    setBriefError(null);
    setBriefing(false);
    // Open on the overview when there is one. Generating a brief is a
    // deliberate act — if the user paid for it, it is what they came back for.
    // Notes without one open on the transcript, which is every note by default.
    setPane(transcript.brief ? "overview" : "transcript");
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
  // happened to live, in whatever format it happened to be — Discord's Opus
  // among them, which the webview can't decode. Pull them in on first open.
  //
  // Library files get the same treatment when they aren't in a format the
  // webview plays. Builds that shipped with the `ffmpeg` transcode never found
  // ffmpeg on a user's machine and archived raw copies instead, so an existing
  // library can be full of files that decode fine and play as nothing. Opening
  // the note re-encodes it, once.
  useEffect(() => {
    if (
      transcript.source_path.includes("/media/") &&
      playableAudio(transcript.source_path)
    ) {
      return;
    }
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
  const placed = useMemo(
    () => paragraphs.map((p) => placeWords(p.text, p.words ?? [])),
    [paragraphs],
  );

  /**
   * What each paragraph renders as — now always its own text, because that is
   * what `placeWords` reconstructs character for character.
   */
  const rendered = useMemo(() => paragraphs.map((p) => p.text), [paragraphs]);

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

    // Already known: `placed` carries each word's range in the paragraph text,
    // measured against the same string `matches` searched.
    const spansFor = (pi: number) => placed[pi] ?? [];

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
  }, [matches, matchAt, paragraphs, placed]);

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
        // Find searches the prose. Opening it from the overview would count
        // and step through matches nobody can see.
        setPane("transcript");
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

  /** Open the label for editing, and — once per note — ask the model who was
   *  on the call. Asked here rather than on save because most meetings are
   *  never renamed, and a model call to answer a question nobody asked is the
   *  cost the overview is careful not to pay either. */
  const openSpeaker = (at: number, current: string) => {
    setSpeakerError(null);
    setSpeakerDraft(current);
    setNamingSpeakerAt(at);

    if (heardNames !== null || listening) return;
    setListening(true);
    namesInMeeting(transcript.id)
      .then(setHeardNames)
      // Apple Intelligence being off is the common case here, and it is not
      // worth a message: the field below still works, which is the whole point.
      .catch(() => setHeardNames([]))
      .finally(() => setListening(false));
  };

  const closeSpeaker = () => {
    setNamingSpeakerAt(null);
    setSpeakerError(null);
  };

  /** Commit a speaker's name. The backend rewrites every turn, so the answer
   *  is a whole new transcript and the parent swaps it in. */
  const commitSpeaker = async (from: string, picked?: string) => {
    const next = (picked ?? speakerDraft).trim();
    if (!next || next === from) {
      closeSpeaker();
      return;
    }
    try {
      await onRenameSpeaker(transcript.id, from, next);
      closeSpeaker();
    } catch (e) {
      // Kept open, with the reason under it: the name is still in the field,
      // so fixing a stray colon is one keystroke rather than typing it again.
      setSpeakerError(e instanceof Error ? e.message : String(e));
    }
  };

  /**
   * The transcript as prose, for copying, .txt and Markdown.
   *
   * Speaker labels are part of the text here, not decoration around it: a
   * meeting pasted into a document without them reads as one person
   * contradicting themselves. Anything without speakers — every dictation and
   * dropped file — is unchanged.
   */
  const fullText = () =>
    paragraphs
      .map((p) => (p.speaker ? `${p.speaker}: ${p.text}` : p.text))
      .join("\n\n");

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
          // Speakers ride along in the text rather than as a field of their
          // own: the PDF layout takes a stamp and a paragraph, and a meeting
          // printed without attribution is the same unreadable run-on that
          // copying one without it would be.
          paragraphs: paragraphs.map((p) => ({
            stamp: formatDuration(p.start),
            text: p.speaker ? `${p.speaker}: ${p.text}` : p.text,
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

  const runBrief = async () => {
    setBriefing(true);
    setBriefError(null);
    try {
      setBrief(await generateBrief(transcript.id));
    } catch (e) {
      // The sentence the backend sent is written for a reader; pass it through
      // rather than replacing it with a generic failure.
      setBriefError(String(e));
    } finally {
      setBriefing(false);
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
                    {formatWhen(transcript.created_at)} //{" "}
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

          {/* Shown when an overview is possible, or could be made possible.
              "Apple Intelligence is switched off" belongs on screen because it
              has a fix in it; "this Mac is too old" does not, and a tab that
              can only ever apologise is worse than no tab. A note that already
              has a brief always keeps its tab — it was readable yesterday and
              nothing about the machine should take that away. */}
          {(canBrief?.available ||
            canBrief?.reason === "apple-intelligence-off" ||
            canBrief?.reason === "model-not-ready" ||
            brief) && (
            <div className="reading-body no-drag flex items-center gap-px pb-2">
              <PaneTab
                label="AI OVERVIEW"
                active={pane === "overview"}
                onClick={() => setPane("overview")}
              />
              <PaneTab
                label="TRANSCRIPT"
                active={pane === "transcript"}
                onClick={() => setPane("transcript")}
              />
            </div>
          )}
        </header>

        {/* The gutter is 40px of stamp and 14px of gap, matching the PDF
            exactly, so a printed transcript and the one on screen have the same
            anatomy. Bottom padding clears the floating player, which overlays
            the text. */}
        <article className="reading-body pb-32 pt-9">
          {pane === "overview" && (
            <Overview
              brief={brief}
              busy={briefing}
              stage={briefStage}
              capability={canBrief}
              error={briefError}
              onGenerate={runBrief}
            />
          )}

          {/* Hidden rather than unmounted. The paragraph refs drive follow-along
              scrolling, the focus tween and find-in-transcript, and tearing them
              down every time someone glances at the overview would reset the
              reader's place and re-run the whole GSAP setup on the way back.
              ~1.75 leading on a ~66-character measure. Font size and opacity are
              owned by GSAP, not React — see the focus effect above; declaring
              them here too would let a re-render stomp mid-tween. */}
          <div
            hidden={pane === "overview"}
            className="space-y-[1.4em] leading-[1.75] text-ink"
          >
            {paragraphs.map((p, pi) => (
              <div key={pi} className="relative">
                {/* Who is talking. Only meetings carry this, and because
                    consecutive turns from one speaker are already merged into
                    one paragraph, every label here is a genuine change of
                    voice — so there is never a run of identical labels to
                    suppress. */}
                {p.speaker &&
                  (namingSpeakerAt === pi ? (
                    <div className="mb-1.5">
                      <input
                        autoFocus
                        value={speakerDraft}
                        onChange={(e) => setSpeakerDraft(e.target.value)}
                        onBlur={() => commitSpeaker(p.speaker!)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") e.currentTarget.blur();
                          if (e.key === "Escape") {
                            // Straight to closed, not through the blur handler,
                            // or escape would save what it was meant to discard.
                            closeSpeaker();
                          }
                        }}
                        aria-label="Name this speaker"
                        className="micro no-drag selectable w-44 border border-sage-dim bg-transparent px-1 py-0.5 uppercase text-ink outline-none"
                      />

                      {/* Names actually said out loud during the call. Offered,
                          never applied: the tap hears the far side as one
                          stream, so knowing Rupesh and Priya were both there
                          says nothing about which voice this is. */}
                      {listening && (
                        <p className="mono-data mt-1 text-[10px] uppercase tracking-[0.12em] text-faint">
                          LISTENING FOR NAMES…
                        </p>
                      )}
                      {suggestions.length > 0 && (
                        <div className="mt-1.5 flex flex-wrap items-center gap-1">
                          {suggestions.map((name) => (
                            <button
                              key={name}
                              // Before blur, or the input would commit and close
                              // out from under the click that landed here.
                              onMouseDown={(e) => {
                                e.preventDefault();
                                setSpeakerDraft(name);
                                commitSpeaker(p.speaker!, name);
                              }}
                              className="micro no-drag border border-hairline px-1.5 py-0.5 text-faint transition-colors hover:border-sage-dim hover:text-ink"
                            >
                              {name.toUpperCase()}
                            </button>
                          ))}
                        </div>
                      )}

                      {speakerError && (
                        <p className="mono-data mt-1 text-[10px] uppercase tracking-[0.12em] text-amber">
                          {speakerError}
                        </p>
                      )}
                    </div>
                  ) : (
                    /* A button, because the tap hears the whole far side as one
                       stream and calls it "Others" — true, and useless the
                       moment you want to know who agreed to something. You know
                       who was talking; this is the cheapest way to get that
                       into the note. Renames every turn by this speaker. */
                    <button
                      onClick={() => openSpeaker(pi, p.speaker!)}
                      title={`Rename ${p.speaker} throughout this meeting`}
                      className={[
                        "micro no-drag mb-1.5 -ml-1 border border-transparent px-1 py-0.5 transition-colors hover:border-hairline hover:text-ink",
                        // By side, not by name. Meetings recorded before sides
                        // were stored fall back to the label they shipped with.
                        (p.side ?? (p.speaker === "You" ? "you" : "others")) ===
                        "you"
                          ? "text-sage-dim"
                          : "text-faint",
                      ].join(" ")}
                    >
                      {p.speaker.toUpperCase()}
                    </button>
                  ))}

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
                      ? placed[pi]?.map((lay, wi) => {
                          const w = p.words![wi];
                          const key = `${pi}:${wi}`;
                          const lit = key === activeKey || key === flash;
                          const hit = marked.all.has(key);
                          const here = marked.current.has(key);
                          return (
                            <Fragment key={wi}>
                            {/* The gap between words is text, not a span —
                                so a cursor selection copies the paragraph as
                                written rather than one space per token. */}
                            {lay.prefix}
                            <span
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
                              {lay.text}
                            </span>
                            </Fragment>
                          );
                        })
                      : p.text}
                  </p>
                )}
              </div>
            ))}
          </div>

          {!hasWords && pane === "transcript" && (
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

/** One half of the Overview/Transcript switch. */
function PaneTab({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={[
        "micro border px-2.5 py-1.5 transition-colors",
        active
          ? "border-ink bg-ink text-surface"
          : "border-hairline text-grey hover:border-ink hover:text-ink",
      ].join(" ")}
    >
      {label}
    </button>
  );
}

/**
 * The structured half of a note: what it was about, rather than what was said.
 *
 * Generated on request and then kept, so this is one of three states — never
 * asked, asking, or answered — and the empty one has to explain itself. A blank
 * pane with a button reads like something is broken.
 */
function Overview({
  brief,
  busy,
  stage,
  capability,
  error,
  onGenerate,
}: {
  brief: Brief | null;
  busy: boolean;
  stage: BriefProgress | null;
  capability: BriefCapability | null;
  error: string | null;
  onGenerate: () => void;
}) {
  if (!brief) {
    // Nothing can be generated yet, and the reason is the only useful thing on
    // screen. The button is still rendered rather than removed, because the
    // reasons that reach here are the ones the user can clear — the pane is
    // hidden entirely for the ones they can't.
    const blocked = capability !== null && !capability.available;
    // Blocked, but by something that clears itself or that the user can clear.
    // Both mean the model that will eventually write this is the local one.
    const pending =
      capability?.reason === "model-not-ready" ||
      capability?.reason === "apple-intelligence-off";

    return (
      <div className="flex flex-col items-start gap-3 border border-hairline bg-panel p-6">
        <p className="eyebrow text-ink">
          {!blocked
            ? "NO AI OVERVIEW YET"
            : capability?.reason === "model-not-ready"
              ? "NOT READY YET"
              : "AI OVERVIEWS ARE OFF"}
        </p>
        <p className="max-w-[440px] text-[13px] leading-relaxed text-grey">
          An overview pulls the summary, key points, decisions and any action
          items out of this note. Meetings write their own as they save; every
          other note waits until you ask, because most dictation is a sentence
          on its way somewhere else and has nothing to summarise.
        </p>
        {/* Where it runs is the part worth saying out loud. It is the whole
            difference between this and every other meeting-notes tool, and the
            place a person would otherwise assume the transcript went.
            Shown while blocked too, and deliberately: waiting for a download or
            walking to System Settings is a cost, and this is the reason it is
            worth paying. Not shown when the overview would come from the
            assistant build's own model, where it would simply be untrue. */}
        {(capability?.on_device || (blocked && pending)) && (
          <p className="max-w-[440px] text-[13px] leading-relaxed text-grey">
            It is written by the model built into macOS, on this Mac. Nothing is
            uploaded and nothing is charged for.
          </p>
        )}
        {blocked && capability.message && (
          <p className="max-w-[440px] text-[13px] leading-relaxed text-amber">
            {capability.message}
          </p>
        )}
        <button
          onClick={onGenerate}
          disabled={busy || blocked}
          className="micro border border-hairline px-2.5 py-1.5 text-grey transition-colors hover:border-ink hover:bg-ink hover:text-surface disabled:opacity-50 disabled:hover:border-hairline disabled:hover:bg-transparent disabled:hover:text-grey"
        >
          {busy ? briefBusyLabel(stage) : "GENERATE"}
        </button>
        {/* A long meeting is read in pieces. The bar is what says the wait is
            finite — the label alone can sit on "reading part 3 of 9" for long
            enough to look stuck. */}
        {busy && stage && (
          <div className="flex w-full max-w-[440px] gap-px">
            {Array.from({ length: 32 }, (_, i) => (
              <span
                key={i}
                className={[
                  "h-1 flex-1",
                  i / 32 < stage.progress ? "bg-sage-dim" : "bg-hairline",
                ].join(" ")}
              />
            ))}
          </div>
        )}
        {/* Prose, not the uppercase mono the other readouts use. That style is
            for codes and counts — a two-line sentence set in tracked-out caps
            is markedly harder to read, and this is the one line here whose job
            is to be read carefully and acted on. */}
        {error && (
          <p className="max-w-[440px] text-[13px] leading-relaxed text-amber">
            {error}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className="space-y-7">
      <p className="text-[17px] leading-[1.65] text-ink">{brief.summary}</p>

      <BriefList title="KEY POINTS" items={brief.key_points} />
      <BriefList title="DECISIONS" items={brief.decisions} />

      {brief.action_items.length > 0 && (
        <section>
          <p className="eyebrow mb-2 text-faint">ACTION ITEMS</p>
          <ul className="space-y-2">
            {brief.action_items.map((a, i) => (
              <li key={i} className="flex gap-3 text-[15px] leading-[1.6]">
                <span className="mt-[7px] h-1 w-1 shrink-0 bg-sage-dim" />
                <span className="text-ink">
                  {a.text}
                  {a.owner && (
                    <span className="mono-data ml-2 text-[10px] uppercase tracking-[0.12em] text-faint">
                      {a.owner}
                    </span>
                  )}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <div className="flex items-center gap-3 border-t border-hairline pt-4">
        <button
          onClick={onGenerate}
          disabled={busy || (capability !== null && !capability.available)}
          className="micro border border-hairline px-2.5 py-1.5 text-grey transition-colors hover:border-ink hover:bg-ink hover:text-surface disabled:opacity-50"
        >
          {busy ? briefBusyLabel(stage) : "REGENERATE"}
        </button>
        {/* Prose here too, and for the same reason as the empty state. */}
        {error && (
          <span className="text-[13px] leading-relaxed text-amber">{error}</span>
        )}
      </div>
    </div>
  );
}

/**
 * What the button says while the model is reading.
 *
 * The stage comes from the backend because only it knows how many passes a note
 * takes — a dictated note is one, a long meeting is a dozen — and "reading part
 * 4 of 12" is the difference between a wait a person will sit through and one
 * they assume has hung. Falls back to the old wording until the first pass
 * reports, which on a short note may be the only thing ever shown.
 */
function briefBusyLabel(stage: BriefProgress | null): string {
  return stage?.stage ? stage.stage.toUpperCase() + "…" : "READING THE NOTE…";
}

/** A titled list that disappears rather than showing an empty heading. */
function BriefList({ title, items }: { title: string; items: string[] }) {
  if (!items.length) return null;
  return (
    <section>
      <p className="eyebrow mb-2 text-faint">{title}</p>
      <ul className="space-y-2">
        {items.map((t, i) => (
          <li key={i} className="flex gap-3 text-[15px] leading-[1.6] text-ink">
            <span className="mt-[7px] h-1 w-1 shrink-0 bg-hairline" />
            <span>{t}</span>
          </li>
        ))}
      </ul>
    </section>
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
