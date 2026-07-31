import { useEffect, useMemo, useState } from "react";

import { analyticsSummary, type Count, type Insights as Data } from "../lib/api";

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
          <span className="mono-data w-[86px] shrink-0 text-right text-[11px] text-grey">
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

  return (
    <div className="overflow-x-auto">
      <div className="flex gap-[3px]">
        {weeks.map((week, i) => (
          <div key={i} className="flex flex-col gap-[3px]">
            {week.map((d) => (
              <span
                key={d.date}
                title={d.words >= 0 ? `${d.date} — ${d.words} words` : undefined}
                className="h-[11px] w-[11px]"
                style={{ background: shade(d.words) }}
              />
            ))}
          </div>
        ))}
      </div>
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

const toRows = (counts: Count[]) =>
  counts.map((c) => ({ label: c.label, value: c.notes, sub: `${c.words}w` }));

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

        <Panel
          title="ACTIVITY"
          aside={`${data.by_day.length} ACTIVE DAYS`}
        >
          <Heatmap days={data.by_day} />
        </Panel>

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

          <Panel title="WHEN YOU SPEAK">
            <Hours by={data.by_hour} />
          </Panel>
        </div>

        <div className="grid gap-3 md:grid-cols-2">
          <Panel title="HOW IT ARRIVES">
            <Bars rows={toRows(data.by_source)} />
          </Panel>

          <Panel title="HOW YOU SPEAK">
            <dl className="space-y-2 text-[12px]">
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
              <p className="micro mt-3 text-faint">
                {v.fillers.map((f) => `${f.word} ${f.count}`).join(" · ")}
              </p>
            )}
          </Panel>
        </div>

        <Panel title="WHAT YOU TALK ABOUT" aside="COMMON WORDS REMOVED">
          {v.top_words.length ? (
            <div className="flex flex-wrap gap-x-3 gap-y-1.5">
              {v.top_words.map((w) => (
                <span key={w.word} className="text-[13px] text-ink">
                  {w.word}
                  <span className="mono-data ml-1 text-[10px] text-faint">
                    {w.count}
                  </span>
                </span>
              ))}
            </div>
          ) : (
            <p className="micro text-faint">NOT ENOUGH TEXT YET</p>
          )}
        </Panel>

        <p className="micro pb-2 text-faint">
          COMPUTED ON THIS MAC FROM YOUR HISTORY // NOTHING IS UPLOADED
        </p>
      </div>
    </div>
  );
}
