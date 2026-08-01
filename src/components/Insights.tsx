import { useEffect, useMemo, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";

import {
  analyticsSummary,
  writeBinaryFile,
  type Count,
  type Insights as Data,
  type WordCount,
} from "../lib/api";
import { renderWordCloud } from "../lib/share";

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
  aside?: string;
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
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  const share = async () => {
    setError(null);
    const target = await save({
      defaultPath: "what-i-talk-about.png",
      filters: [{ name: "PNG image", extensions: ["png"] }],
    });
    if (!target) return;
    setSaving(true);
    try {
      const png = await renderWordCloud({
        words: shown.map((w) => ({ word: w.word, count: w.count })),
        notes,
        totalWords,
        period,
      });
      await writeBinaryFile(target, png);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
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

          <div className="mt-4 flex items-center gap-3 border-t border-hairline pt-3">
            <button
              onClick={share}
              disabled={saving}
              className="micro border border-ink bg-ink px-3 py-1.5 text-surface transition-colors hover:bg-transparent hover:text-ink disabled:opacity-50"
            >
              {saving ? "RENDERING…" : "SAVE AS IMAGE"}
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
          {error && (
            <p className="mono-data mt-2 text-[10px] uppercase tracking-[0.12em] text-amber">
              COULD NOT SAVE — {error}
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

  useEffect(() => {
    analyticsSummary()
      .then(setData)
      .catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return (
      <div className="titlebar-pad flex h-full items-center justify-center p-8">
        <p className="micro text-amber">COULD NOT READ HISTORY — {error}</p>
      </div>
    );
  }
  if (!data) {
    return (
      <div className="titlebar-pad flex h-full items-center justify-center p-8">
        <p className="micro text-faint">READING HISTORY…</p>
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
      <div className="mx-auto max-w-[860px] space-y-3 p-6">
        <header>
          <h1 className="text-[22px] font-semibold tracking-[-0.01em] text-ink">
            Insights
          </h1>
          <p className="eyebrow mt-1 text-faint">
            {data.first_day && data.last_day
              ? `${data.first_day} → ${data.last_day} // ${data.total_notes} NOTES`
              : `${data.total_notes} NOTES`}
          </p>
        </header>

        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
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
        <div className="grid gap-3 md:grid-cols-2">
          <Panel title="ACTIVITY" aside={`${data.by_day.length} ACTIVE DAYS`}>
            <Heatmap days={data.by_day} />
          </Panel>

          <Panel title="WHEN YOU SPEAK">
            <Hours by={data.by_hour} />
          </Panel>
        </div>

        <div className="grid gap-3 md:grid-cols-2">
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

        <div className="grid gap-3">
          <Panel title="HOW YOU SPEAK">
            {/* Two columns across the full width: four rows stacked in one
                column left a panel mostly made of gap. */}
            <dl className="grid gap-x-10 gap-y-2 text-[12px] sm:grid-cols-2">
              <div className="flex justify-between gap-3">
                <dt className="text-grey">Average sentence</dt>
                <dd className="mono-data text-ink">
                  {v.avg_sentence_words.toFixed(1)} words
                </dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-grey">Longest sentence</dt>
                <dd className="mono-data text-ink">
                  {v.longest_sentence_words} words
                </dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-grey">Distinct words</dt>
                <dd className="mono-data text-ink">
                  {v.unique_words.toLocaleString()}
                </dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-grey">Filler rate</dt>
                <dd className="mono-data text-ink">
                  {v.filler_rate.toFixed(1)} per 100 words
                </dd>
              </div>
            </dl>
            {v.fillers.length > 0 && (
              // Which fillers, and how often. Unlabelled, this read as a bare
              // "I MEAN 1" hanging under the rate — a fragment of a sentence
              // rather than a count. The heading and the × make it a count.
              <p className="micro mt-3 text-faint">
                <span className="text-grey">WHICH ONES</span>{" "}
                {v.fillers.map((f) => `${f.word} ×${f.count}`).join(" · ")}
              </p>
            )}
          </Panel>
        </div>

        <WordCloud
          words={v.top_words}
          notes={data.total_notes}
          totalWords={data.total_words}
          period={period(data.first_day, data.last_day)}
        />

        <p className="micro pb-2 text-faint">
          COMPUTED ON THIS MAC FROM YOUR HISTORY // NOTHING IS UPLOADED
        </p>
      </div>
    </div>
  );
}
