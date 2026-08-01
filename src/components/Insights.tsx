import { useEffect, useMemo, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import gsap from "gsap";

import {
  analyticsSummary,
  writeBinaryFile,
  type Count,
  type Insights as Data,
  type WordCount,
} from "../lib/api";
import { EASE, prefersReducedMotion } from "../lib/motion";
import { bestType, renderReel } from "../lib/reel";
import { renderWordCloud } from "../lib/share";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

/**
 * Insights — what the history says about how you speak.
 *
 * Two things this view refuses to do, both of which are the normal behaviour of
 * analytics screens:
 *
 * **It won't print a confident number from a thin sample.** Words-per-minute
 * off nine seconds of audio is noise. Under `MIN_SAMPLE` the rate is withheld
 * and the panel says how much more speech it needs, because a wrong 143 WPM is
 * worse than an honest blank.
 *
 * **It won't quietly drop what it can't measure.** Dictations recorded before
 * app capture existed have no app name. They're shown as "not recorded" in the
 * chart rather than silently excluded, so the bars can't imply a coverage they
 * don't have.
 */

/** Below two minutes of speech, a words-per-minute figure is not a fact. */
const MIN_SAMPLE = 120;

function hours(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return m ? `${h}h ${m}m` : `${h}h`;
}

function Stat({
  value,
  label,
  note,
}: {
  value: string;
  label: string;
  note?: string;
}) {
  return (
    <div className="border border-hairline bg-panel p-4">
      <p className="mono-data text-[28px] leading-none text-ink">{value}</p>
      <p className="eyebrow mt-2 text-faint">{label}</p>
      {note && <p className="micro mt-1 text-grey">{note}</p>}
    </div>
  );
}

function Panel({
  title,
  aside,
  children,
}: {
  title: string;
  aside?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className="border border-hairline bg-panel p-4">
      <div className="mb-3 flex items-baseline justify-between gap-3">
        <h2 className="eyebrow text-ink">{title}</h2>
        {aside && <span className="micro shrink-0 text-faint">{aside}</span>}
      </div>
      {children}
    </section>
  );
}

/** Horizontal bars. Widths are relative to the largest row, not to the total,
 *  so a dominant first place doesn't flatten everything below it into slivers. */
function Bars({
  rows,
  unit = "notes",
}: {
  rows: { label: string; value: number; sub?: string }[];
  unit?: string;
}) {
  const max = Math.max(1, ...rows.map((r) => r.value));
  if (!rows.length) {
    return <p className="micro text-faint">NOTHING RECORDED YET</p>;
  }
  return (
    <ul className="space-y-1.5">
      {rows.map((r) => (
        <li key={r.label} className="flex items-center gap-3">
          <span className="w-[104px] shrink-0 truncate text-[12px] text-ink" title={r.label}>
            {r.label}
          </span>
          <span className="relative h-4 flex-1 bg-hairline-soft">
            <span
              className="absolute inset-y-0 left-0 bg-sage-dim"
              style={{ width: `${(r.value / max) * 100}%` }}
            />
          </span>
          {/* Fixed width keeps every bar ending on the same line, so this must
              never wrap: a three-digit word count used to push "335w" onto a
              second row and knock that one bar out of alignment with the rest.
              `compact` bounds the string so a busy month can't do it again. */}
          <span className="mono-data w-[104px] shrink-0 whitespace-nowrap text-right text-[11px] text-grey">
            {r.value} {unit}
            {r.sub ? ` · ${r.sub}` : ""}
          </span>
        </li>
      ))}
    </ul>
  );
}

/**
 * Activity grid. Columns are weeks, rows are weekdays — the shape everyone
 * already knows how to read, so it needs no legend beyond less/more.
 */
function Heatmap({ days }: { days: Data["by_day"] }) {
  const { weeks, max } = useMemo(() => {
    const counts = new Map(days.map((d) => [d.date, d.words]));
    const today = new Date();
    // Start 17 weeks back, snapped to the Sunday, so columns are whole weeks.
    const start = new Date(today);
    start.setDate(start.getDate() - 7 * 17 - today.getDay());

    const cols: { date: string; words: number }[][] = [];
    const cursor = new Date(start);
    while (cursor <= today) {
      const week: { date: string; words: number }[] = [];
      for (let d = 0; d < 7; d++) {
        // Local ISO date; toISOString() would shift by the UTC offset and
        // slide every square onto the wrong day for anyone west of London.
        const iso = `${cursor.getFullYear()}-${String(cursor.getMonth() + 1).padStart(2, "0")}-${String(cursor.getDate()).padStart(2, "0")}`;
        week.push({ date: iso, words: cursor <= today ? (counts.get(iso) ?? 0) : -1 });
        cursor.setDate(cursor.getDate() + 1);
      }
      cols.push(week);
    }
    return { weeks: cols, max: Math.max(1, ...days.map((d) => d.words)) };
  }, [days]);

  const shade = (words: number) => {
    if (words < 0) return "transparent";
    if (words === 0) return "var(--color-hairline-soft)";
    const t = Math.min(1, words / max);
    // Four steps rather than a continuous ramp: adjacent continuous values are
    // indistinguishable at 11px, so the gradient would read as noise.
    const step = t > 0.66 ? 1 : t > 0.33 ? 0.72 : 0.44;
    return `color-mix(in srgb, var(--color-sage-dim) ${step * 100}%, transparent)`;
  };

  // Columns are sized by the panel rather than fixed at 11px. At a fixed size
  // the grid drew a ~250px block into an 860px panel and left two-thirds of the
  // row empty — the squares now grow to fill whatever width they are given, so
  // the panel is the size of its contents in both directions.
  return (
    <div
      className="grid w-full gap-[3px]"
      style={{
        gridTemplateRows: "repeat(7, 1fr)",
        gridAutoColumns: "1fr",
        gridAutoFlow: "column",
      }}
    >
      {weeks.flat().map((d) => (
        <span
          key={d.date}
          title={d.words >= 0 ? `${d.date} — ${d.words} words` : undefined}
          className="aspect-square w-full"
          style={{ background: shade(d.words) }}
        />
      ))}
    </div>
  );
}

/** When you speak, across the day. */
function Hours({ by }: { by: number[] }) {
  const max = Math.max(1, ...by);
  return (
    <div>
      <div className="flex h-16 items-end gap-[2px]">
        {by.map((n, h) => (
          <span
            key={h}
            title={`${String(h).padStart(2, "0")}:00 — ${n} notes`}
            className="flex-1 bg-sage-dim"
            style={{ height: `${Math.max(n ? 6 : 1, (n / max) * 100)}%` }}
          />
        ))}
      </div>
      <div className="mono-data mt-1 flex justify-between text-[9px] text-faint">
        <span>00</span>
        <span>06</span>
        <span>12</span>
        <span>18</span>
        <span>23</span>
      </div>
    </div>
  );
}

/**
 * How you speak.
 *
 * Four label-and-number rows in a full-width panel was mostly leader space —
 * the eye had to travel the whole panel to pair a name with its figure. These
 * are four measurements of one thing, so they read as four measures: the number
 * first at size, the name under it, and the fillers given a line of their own
 * because a list of your verbal tics is the interesting part, not a footnote.
 */
function Speech({ v }: { v: Data["vocabulary"] }) {
  const measures = [
    { value: v.avg_sentence_words.toFixed(1), unit: "words", label: "AVERAGE SENTENCE" },
    { value: String(v.longest_sentence_words), unit: "words", label: "LONGEST SENTENCE" },
    { value: v.unique_words.toLocaleString(), unit: "", label: "DISTINCT WORDS" },
    { value: v.filler_rate.toFixed(1), unit: "per 100 words", label: "FILLER RATE" },
  ];

  return (
    <>
      <div className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-4">
        {measures.map((m) => (
          <div key={m.label}>
            <p className="mono-data text-[22px] leading-none text-ink">
              {m.value}
              {m.unit && (
                <span className="ml-1 text-[11px] text-grey">{m.unit}</span>
              )}
            </p>
            <p className="eyebrow mt-2 text-faint">{m.label}</p>
          </div>
        ))}
      </div>

      {v.fillers.length > 0 && (
        <div className="mt-5 border-t border-hairline pt-3">
          <p className="eyebrow mb-2 text-faint">WHICH FILLERS</p>
          <div className="flex flex-wrap gap-x-2 gap-y-1.5">
            {v.fillers.map((f) => (
              <span
                key={f.word}
                className="flex items-baseline gap-1.5 border border-hairline bg-surface px-2 py-1"
              >
                <span className="text-[12px] text-ink">{f.word}</span>
                <span className="mono-data text-[10px] text-grey">×{f.count}</span>
              </span>
            ))}
          </div>
        </div>
      )}
    </>
  );
}

/**
 * Filler rate, against the only published figure worth citing.
 *
 * Bortfeld et al. (2001) measured 192 speakers in recorded conversation and
 * reported 3.04 fillers per 100 words for men and 2.07 for women — a real
 * baseline rather than a number invented to be flattering.
 *
 * The caveat is on the card because it has to be: our detection is deliberately
 * conservative — [`FILLERS`] counts eight unambiguous ones and skips "like",
 * "just" and "so" precisely because they do ordinary work — so this counts fewer
 * things than the study did. Everyone will therefore score low against it. Said
 * plainly, that is context; unsaid, it would be flattery.
 */
function FillerRate({ v }: { v: Data["vocabulary"] }) {
  const MAX = 6;
  const LOW = 2.07;
  const HIGH = 3.04;
  const at = (n: number) => `${Math.max(0, Math.min(100, (n / MAX) * 100))}%`;
  // The two captions sit on opposite sides of the rail, so yours can never
  // collide with the band's however close the numbers happen to be. Yours is
  // also held off the ends, where a centred label would hang outside the card.
  const you = `${Math.max(6, Math.min(94, (v.filler_rate / MAX) * 100))}%`;
  const band = at((LOW + HIGH) / 2);

  return (
    // "ALL TIME" is doing real work: the panel beside this one shows the same
    // measure over its later window, so without it the two cards look like the
    // same number disagreeing with itself.
    <Panel title="HOW OFTEN YOU REACH FOR A FILLER" aside="ALL TIME">
      <p className="flex items-baseline gap-1.5">
        <span className="mono-data text-[34px] leading-none text-ink">
          {v.filler_rate.toFixed(1)}
        </span>
        <span className="text-[12px] text-grey">per 100 words</span>
      </p>

      <div className="relative mt-7 h-[44px]">
        {/* The published band, not a target — labelled above the rail. */}
        <span
          className="diagnostic absolute top-0 -translate-x-1/2 whitespace-nowrap text-faint"
          style={{ left: band }}
        >
          TYPICAL
        </span>
        <span className="absolute left-0 right-0 top-[20px] h-[2px] bg-hairline-soft" />
        <span
          className="absolute top-[16px] h-[10px] bg-sage-dim/35"
          style={{ left: at(LOW), width: at(HIGH - LOW) }}
        />
        <span
          className="absolute top-[11px] h-[20px] w-[2px] bg-sage-dim"
          style={{ left: at(v.filler_rate) }}
        />
        <span
          className="diagnostic absolute top-[34px] -translate-x-1/2 whitespace-nowrap text-sage-dim"
          style={{ left: you }}
        >
          YOU
        </span>
      </div>

      <p className="micro flex justify-between text-faint">
        <span>0</span>
        <span>{MAX} PER 100 WORDS</span>
      </p>

      <p className="mt-5 border-t border-hairline pt-3 text-[11.5px] leading-snug text-grey">
        Typical is <b className="text-ink">2.07–3.04</b> (Bortfeld et al., 2001,
        n=192). We count only unmistakable fillers — never “like” or “just” — so
        this runs low against that study by design.
      </p>
    </Panel>
  );
}

/**
 * The spans on offer, shortest first.
 *
 * `needs` names what a disabled tab is missing — always the *earlier* of its
 * two windows, since that is the side a new history hasn't reached yet.
 */
const SPANS: { key: string; label: string; needs: string }[] = [
  { key: "1", label: "1D", needs: "the day before that" },
  { key: "7", label: "7D", needs: "the week before that" },
  { key: "30", label: "30D", needs: "the month before that" },
  { key: "all", label: "ALL", needs: "" },
];

/**
 * How long each half is, said in the unit that distinguishes it.
 *
 * Printed on every span, not just ALL, because two spans can land on the same
 * partition: two days of history makes ALL a 23-hour half, and if nothing was
 * recorded in the hour between that cut and the 1D cut, both tabs group exactly
 * the same notes and show exactly the same figures. That is arithmetic, not a
 * bug — but without the window length on the card it reads as one tab ignoring
 * you, so the card says where it cut.
 */
function halves(hours: number): string {
  if (hours >= 48) return `${Math.round(hours / 24)}-DAY HALVES`;
  return `${hours}-HOUR HALVES`;
}

/**
 * Whether you are getting better, measured against yourself.
 *
 * There is no other honest baseline: the app holds nobody else's speech and
 * collects none. Two equal windows of your own history, the same measurements
 * on each. It refuses to report anything until both windows carry enough words
 * to be stable, because a filler rate off eighty words is noise and calling
 * noise "progress" is what makes habit trackers untrustworthy.
 *
 * Every span is computed up front, so switching between them is a re-render
 * rather than a round trip and can be animated without a spinner in the middle.
 * A span the history can't support is shown disabled with what it needs, not
 * hidden — otherwise the control silently changes shape as the weeks pass.
 */
function Trend({ windows }: { windows: Data["progress"] }) {
  // Value and unit are kept apart so the figure can be set large and the unit
  // small. Run together — "1.5/100w" at reading size — four of these collided
  // with the labels beside them and the row broke into the next column.
  const NAMES: Record<
    string,
    { label: string; value: (n: number) => string; unit: string }
  > = {
    filler_rate: { label: "FILLER RATE", value: (n) => n.toFixed(1), unit: "per 100w" },
    variety: { label: "WORD VARIETY", value: (n) => String(Math.round(n * 100)), unit: "%" },
    avg_sentence_words: { label: "SENTENCE LENGTH", value: (n) => n.toFixed(1), unit: "words" },
    words_per_minute: { label: "SPEAKING PACE", value: (n) => String(Math.round(n)), unit: "wpm" },
  };

  const [picked, setPicked] = useState<string | null>(null);
  const body = useRef<HTMLDivElement>(null);

  // The chosen span, or the shortest one with something to say. A day is what
  // people ask about first, but opening on "not yet" reads as broken, so a span
  // that actually holds a comparison wins over an emptier shorter one.
  const p =
    (picked ? windows.find((w) => w.key === picked) : undefined) ??
    windows.find((w) => w.available && w.ready) ??
    windows.find((w) => w.available) ??
    windows[0];

  // Switching spans replays the figures rather than swapping them in place: the
  // numbers change meaning entirely between windows, and a silent substitution
  // makes it look like your speech changed rather than the question.
  useEffect(() => {
    const el = body.current;
    if (!el || prefersReducedMotion()) return;
    const tween = gsap.fromTo(
      el.querySelectorAll("[data-move]"),
      { opacity: 0, y: 10 },
      { opacity: 1, y: 0, duration: 0.34, ease: EASE.snap, stagger: 0.05 },
    );
    return () => {
      tween.kill();
    };
  }, [p?.key, p?.ready]);

  if (!p) return null;

  const tabs = (
    <div className="flex items-center gap-1">
      {SPANS.map((s) => {
        const w = windows.find((x) => x.key === s.key);
        const on = w?.key === p.key;
        const can = !!w?.available;
        return (
          <button
            key={s.key}
            type="button"
            disabled={!can}
            onClick={() => setPicked(s.key)}
            title={can ? undefined : `Nothing recorded in ${s.needs}`}
            className={`micro border px-1.5 py-[3px] transition-colors ${
              on
                ? "border-ink bg-ink text-surface"
                : can
                  ? "border-hairline text-grey hover:border-ink hover:text-ink"
                  : "cursor-not-allowed border-transparent text-faint/45"
            }`}
          >
            {s.label}
          </button>
        );
      })}
    </div>
  );

  return (
    <Panel title="ARE YOU IMPROVING" aside={tabs}>
      <div ref={body}>
        {!p.ready ? (
          <>
            <p data-move className="text-[12px] leading-relaxed text-grey">
              This compares two equal stretches of your own history. There isn’t
              enough speech in both yet — keep dictating and it fills itself in.
            </p>
            <p data-move className="micro mt-3 text-faint">
              {p.before_words.toLocaleString()} THEN ·{" "}
              {p.after_words.toLocaleString()} NOW · NEEDS 250 EACH
            </p>
          </>
        ) : (
          <>
            <ul className="grid grid-cols-2 gap-x-5 gap-y-6">
              {p.moves.map((m) => {
                const meta = NAMES[m.key];
                const delta = m.after - m.before;
                const pct = m.before !== 0 ? (delta / m.before) * 100 : 0;
                // No verdict where none is warranted: a longer sentence is not
                // a better or worse one, so those are drawn neutral.
                const judged = m.higher_is_better !== null && Math.abs(pct) >= 1;
                const good = judged && delta > 0 === m.higher_is_better;
                return (
                  <li key={m.key} data-move className="min-w-0">
                    <p className="eyebrow text-faint">{meta.label}</p>
                    <p className="mt-2 flex items-baseline gap-1.5">
                      <span className="mono-data text-[20px] leading-none text-ink">
                        {meta.value(m.after)}
                      </span>
                      <span className="text-[10.5px] text-grey">{meta.unit}</span>
                    </p>
                    <p className="micro mt-2 flex flex-wrap items-baseline gap-x-2 text-faint">
                      <span>FROM {meta.value(m.before)}</span>
                      <span
                        className={
                          !judged
                            ? "text-faint"
                            : good
                              ? "text-sage-dim"
                              : "text-amber"
                        }
                      >
                        {Math.abs(pct) < 1
                          ? "LEVEL"
                          : `${delta > 0 ? "↑" : "↓"}${Math.round(Math.abs(pct))}%`}
                      </span>
                    </p>
                  </li>
                );
              })}
            </ul>
            <p
              data-move
              className="micro mt-5 border-t border-hairline pt-3 text-faint"
            >
              {p.before_words.toLocaleString()} WORDS THEN ·{" "}
              {p.after_words.toLocaleString()} NOW · {halves(p.window_hours)}
            </p>
          </>
        )}
      </div>
    </Panel>
  );
}

/** "2026-07-04" + "2026-08-01" → "Jul – Aug 2026". Shown on the share card. */
function period(first: string | null, last: string | null): string {
  if (!first || !last) return "";
  const fmt = (iso: string) =>
    new Date(`${iso}T12:00:00`).toLocaleDateString(undefined, {
      month: "short",
      year: "numeric",
    });
  const a = fmt(first);
  const b = fmt(last);
  return a === b ? a : `${a} – ${b}`;
}

/**
 * `2026-08-01-1432`, for the export filename.
 *
 * Every save offered the same name, so the second one landed on a "replace?"
 * prompt and the third overwrote a card someone had meant to keep. Local time
 * rather than ISO/UTC: the file is named for the day the person had.
 */
function stamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
}

/** Words the user has struck off their own cloud, per this machine. */
const HIDDEN_KEY = "voicedumps:cloud-hidden";

function loadHidden(): Set<string> {
  try {
    const raw = localStorage.getItem(HIDDEN_KEY);
    return new Set(raw ? (JSON.parse(raw) as string[]) : []);
  } catch {
    return new Set();
  }
}

/**
 * "What you talk about", as something you can post.
 *
 * Two things make this different from the list it replaces. Size carries the
 * frequency, so the shape of a week is legible before any number is read. And
 * every word can be struck out: this is a picture of what someone dictates,
 * which for most people means client names, unreleased products and the odd
 * colleague — one click from public. Removal is the feature, not a nicety, and
 * it is applied to the export as well as to the panel.
 */
function WordCloud({
  words,
  notes,
  totalWords,
  period,
}: {
  words: WordCount[];
  notes: number;
  totalWords: number;
  period: string;
}) {
  const [hidden, setHidden] = useState<Set<string>>(loadHidden);
  const [busy, setBusy] = useState<"png" | "reel" | null>(null);
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  // Probed once: whether this webview has an encoder at all.
  const canRecord = useMemo(() => bestType() !== null, []);

  const shown = useMemo(
    () => words.filter((w) => !hidden.has(w.word)),
    [words, hidden],
  );

  // Functional, because striking three words off quickly is one React batch:
  // building the next set from the render's `hidden` made each click start from
  // the same stale value, and only the last one survived.
  const hide = (word: string) =>
    setHidden((prev) => new Set(prev).add(word));

  const restoreAll = () => setHidden(new Set());

  // Persisting as an effect keeps it correct however the set was reached.
  useEffect(() => {
    try {
      if (hidden.size) {
        localStorage.setItem(HIDDEN_KEY, JSON.stringify([...hidden]));
      } else {
        localStorage.removeItem(HIDDEN_KEY);
      }
    } catch {
      // Storage can throw in a locked-down webview; the words stay hidden for
      // this session either way.
    }
  }, [hidden]);

  // Rank-based, matching the export exactly: counts cluster tightly in a young
  // history, so scaling by count draws every word at nearly one size.
  const scale = (i: number) =>
    i < 2 ? "text-[30px]" : i < 4 ? "text-[25px]" : i < 7 ? "text-[20px]"
      : i < 11 ? "text-[17px]" : i < 17 ? "text-[15px]" : "text-[13px]";

  const card = () => ({
    words: shown.map((w) => ({ word: w.word, count: w.count })),
    notes,
    totalWords,
    period,
  });

  const share = async () => {
    setError(null);
    setNote(null);
    const target = await save({
      defaultPath: `what-i-talk-about-${stamp()}.png`,
      filters: [{ name: "PNG image", extensions: ["png"] }],
    });
    if (!target) return;
    setBusy("png");
    try {
      await writeBinaryFile(target, await renderWordCloud(card()));
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  };

  const shareReel = async () => {
    setError(null);
    setNote(null);
    // The container is whatever the webview can encode, and the extension has
    // to match it or the file is unopenable. So it is decided before the dialog
    // rather than after, and the user picks a name for the format they'll get.
    const kind = bestType();
    const ext = kind?.startsWith("video/mp4") ? "mp4" : "webm";
    const target = await save({
      defaultPath: `what-i-talk-about-${stamp()}.${ext}`,
      filters: [{ name: ext.toUpperCase() + " video", extensions: [ext] }],
    });
    if (!target) return;
    setBusy("reel");
    setProgress(0);
    try {
      const reel = await renderReel(card(), setProgress);
      await writeBinaryFile(target, reel.bytes);
      if (reel.extension !== "mp4") {
        // Better to say so than to let it fail at the upload screen.
        setNote(
          "SAVED AS WEBM — THIS WEBVIEW HAS NO H.264 ENCODER, AND INSTAGRAM WILL NOT ACCEPT IT",
        );
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Panel
      title="WHAT YOU TALK ABOUT"
      aside={hidden.size ? `${hidden.size} HIDDEN` : "CLICK A WORD TO HIDE IT"}
    >
      {shown.length ? (
        <>
          <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
            {shown.map((w, i) => (
              <button
                key={w.word}
                onClick={() => hide(w.word)}
                title={`${w.word} — ${w.count} times. Click to keep it off the card.`}
                className={[
                  scale(i),
                  "font-semibold leading-tight tracking-[-0.015em] transition-colors",
                  i < 2 ? "text-sage-dim" : i < 7 ? "text-ink" : "text-grey",
                  "hover:text-amber hover:line-through",
                ].join(" ")}
              >
                {w.word}
              </button>
            ))}
          </div>

          <div className="mt-4 flex flex-wrap items-center gap-3 border-t border-hairline pt-3">
            <button
              onClick={share}
              disabled={busy !== null}
              className="micro border border-ink bg-ink px-3 py-1.5 text-surface transition-colors hover:bg-transparent hover:text-ink disabled:opacity-50"
            >
              {busy === "png" ? "RENDERING…" : "SAVE AS IMAGE"}
            </button>
            <button
              onClick={shareReel}
              disabled={busy !== null || !canRecord}
              title={
                canRecord
                  ? "A short portrait video of the cloud assembling"
                  : "This build's webview cannot record video"
              }
              className="micro border border-hairline px-3 py-1.5 text-ink transition-colors hover:border-sage-dim disabled:opacity-40"
            >
              {busy === "reel"
                ? `RECORDING ${Math.round(progress * 100)}%`
                : "SAVE AS VIDEO"}
            </button>
            <span className="micro text-faint">1080 × 1920 · PORTRAIT</span>
            {hidden.size > 0 && (
              <button
                onClick={restoreAll}
                className="micro ml-auto text-grey underline-offset-2 hover:text-ink hover:underline"
              >
                RESTORE {hidden.size}
              </button>
            )}
          </div>
          {busy === "reel" && (
            // Recording runs in real time off the canvas, so this is a genuine
            // seven-second wait rather than a progress bar for show.
            <div className="mt-2 h-[2px] w-full bg-hairline-soft">
              <div
                className="h-full bg-sage-dim transition-[width] duration-100"
                style={{ width: `${Math.round(progress * 100)}%` }}
              />
            </div>
          )}
          {error && (
            <p className="mono-data mt-2 text-[10px] uppercase tracking-[0.12em] text-amber">
              COULD NOT SAVE — {error}
            </p>
          )}
          {note && (
            <p className="mono-data mt-2 text-[10px] uppercase tracking-[0.12em] text-amber">
              {note}
            </p>
          )}
        </>
      ) : (
        <p className="micro text-faint">
          {words.length ? "EVERY WORD HIDDEN" : "NOT ENOUGH TEXT YET"}
        </p>
      )}
    </Panel>
  );
}

/** 940 → "940", 1_240 → "1.2k". Keeps the count column a fixed width. */
const compact = (n: number) =>
  n < 1000 ? String(n) : `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;

const toRows = (counts: Count[]) =>
  counts.map((c) => ({
    label: c.label,
    value: c.notes,
    sub: `${compact(c.words)}w`,
  }));

export function Insights() {
  const [data, setData] = useState<Data | null>(null);
  const [error, setError] = useState<string | null>(null);
  const sheet = useRef<HTMLDivElement>(null);

  useEffect(() => {
    analyticsSummary()
      .then(setData)
      .catch((e) => setError(String(e)));
  }, []);

  // The panels arrive in reading order once the numbers are in.
  //
  // Keyed on `data` rather than on mount: the figures are computed on every
  // open, so animating at mount would run the entrance against the loading
  // state and the panels would already be sitting there when it finished.
  useEffect(() => {
    if (!data || !sheet.current) return;
    if (prefersReducedMotion()) return;
    const rows = sheet.current.querySelectorAll("[data-row]");
    const tl = gsap.timeline();
    tl.fromTo(
      rows,
      { opacity: 0, y: 14 },
      {
        opacity: 1,
        y: 0,
        duration: 0.42,
        ease: EASE.snap,
        // Fast enough that the last panel isn't still arriving after the eye
        // has reached it — the whole sequence is under half a second of stagger.
        stagger: 0.055,
      },
    );
    return () => {
      tl.kill();
      // Leave the panels visible if the view is torn down mid-run.
      gsap.set(rows, { clearProps: "opacity,transform" });
    };
  }, [data]);

  if (error) {
    return (
      <div className="titlebar-pad flex h-full items-center justify-center p-8">
        <p className="micro text-amber">COULD NOT READ HISTORY — {error}</p>
      </div>
    );
  }
  if (!data) {
    // Every figure on this screen is computed from the whole history on each
    // open — nothing is cached, so on a large one this is a real wait. The
    // pulsing mark is the app's own working state rather than a spinner.
    return (
      <div className="titlebar-pad flex h-full flex-col items-center justify-center gap-3 p-8">
        <span className="text-sage-dim">
          <PixelCluster pattern={CLUSTERS.brand} size={7} gap={3} pulse />
        </span>
        <p className="micro text-faint">READING YOUR HISTORY…</p>
      </div>
    );
  }
  if (data.total_notes === 0) {
    return (
      <div className="titlebar-pad flex h-full flex-col items-center justify-center gap-2 p-8">
        <p className="eyebrow text-ink">NOTHING TO REPORT YET</p>
        <p className="max-w-[380px] text-center text-[13px] text-grey">
          Dictate with the globe key or drop in a file. Insights fills itself in
          from your history — there is nothing to switch on.
        </p>
      </div>
    );
  }

  const { speaking: sp, vocabulary: v } = data;
  const thin = sp.sample_seconds < MIN_SAMPLE;

  return (
    <div className="titlebar-pad scroll-slim h-full overflow-y-auto">
      <div ref={sheet} className="mx-auto max-w-[860px] space-y-3 p-6">
        <header data-row>
          <h1 className="text-[22px] font-semibold tracking-[-0.01em] text-ink">
            Insights
          </h1>
          <p className="eyebrow mt-1 text-faint">
            {data.first_day && data.last_day
              ? `${data.first_day} → ${data.last_day} // ${data.total_notes} NOTES`
              : `${data.total_notes} NOTES`}
          </p>
        </header>

        <div data-row className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Stat
            value={thin ? "—" : String(Math.round(sp.words_per_minute))}
            label="WORDS PER MINUTE"
            note={
              thin
                ? `NEEDS ${Math.ceil((MIN_SAMPLE - sp.sample_seconds) / 60)} MIN MORE SPEECH`
                : `FROM ${hours(sp.sample_seconds)} OF YOUR VOICE`
            }
          />
          <Stat value={data.total_words.toLocaleString()} label="TOTAL WORDS" />
          <Stat value={hours(data.total_seconds)} label="AUDIO PROCESSED" />
          <Stat
            value={String(data.current_streak)}
            label="DAY STREAK"
            note={`LONGEST ${data.longest_streak}`}
          />
        </div>

        {/* Activity shares its row rather than owning one. A 17-week grid is
            about 250px of content; in a full-width panel that left two-thirds
            of the row empty, and stretching the squares to fill it only made
            the same information take more space. Half width fits it. */}
        <div data-row className="grid gap-3 md:grid-cols-2">
          <Panel title="ACTIVITY" aside={`${data.by_day.length} ACTIVE DAYS`}>
            <Heatmap days={data.by_day} />
          </Panel>

          <Panel title="WHEN YOU SPEAK">
            <Hours by={data.by_hour} />
          </Panel>
        </div>

        <div data-row className="grid gap-3 md:grid-cols-2">
          <Panel
            title="WHERE YOUR VOICE GOES"
            aside={data.app_unknown ? `${data.app_unknown} NOT RECORDED` : undefined}
          >
            <Bars rows={toRows(data.by_app)} />
            {data.by_app.length === 0 && (
              <p className="micro mt-2 text-grey">
                FILLS IN AS YOU DICTATE FROM NOW ON
              </p>
            )}
          </Panel>

          <Panel title="HOW IT ARRIVES">
            <Bars rows={toRows(data.by_source)} />
          </Panel>
        </div>

        <div data-row className="grid gap-3">
          <Panel title="HOW YOU SPEAK">
            <Speech v={v} />
          </Panel>
        </div>

        <div data-row className="grid gap-3 md:grid-cols-2">
          <FillerRate v={v} />
          <Trend windows={data.progress} />
        </div>

        <div data-row>
        <WordCloud
          words={v.top_words}
          notes={data.total_notes}
          totalWords={data.total_words}
          period={period(data.first_day, data.last_day)}
        />
        </div>

        <p className="micro pb-2 text-faint">
          COMPUTED ON THIS MAC FROM YOUR HISTORY // NOTHING IS UPLOADED
        </p>
      </div>
    </div>
  );
}
