import { useEffect, useMemo, useRef, useState } from "react";
import gsap from "gsap";

import type { IngestProgress, Origin, TranscriptMeta } from "../lib/api";
import {
  appVersion,
  checkUpdate,
  getSettings,
  openRelease,
  setLivePreview,
} from "../lib/api";
import { dateGroup, formatDuration, formatRelativeDate } from "../lib/format";
import { EASE, useGsap } from "../lib/motion";
import { useTheme } from "../lib/theme";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

type Props = {
  items: TranscriptMeta[];
  activeId: string | null;
  query: string;
  onQueryChange: (q: string) => void;
  onSelect: (id: string) => void;
  onNew: () => void;
  ingest: IngestProgress | null;
  /** Ids the AI is currently generating a title for. */
  namingIds: Set<string>;
  insightsOpen: boolean;
  onInsights: () => void;
};

/**
 * Origin marks. Two letters in mono rather than an icon: at 9px a glyph is
 * mush, and these sit next to the existing mono metadata line so they read as
 * part of the same readout.
 */
const ORIGIN: Record<Origin, { mark: string; label: string }> = {
  file: { mark: "FL", label: "Dropped file" },
  mic: { mark: "MIC", label: "Recorded in app" },
  hotkey: { mark: "FN", label: "Dictated with the globe key" },
};

const GROUP_ORDER = ["Today", "Yesterday", "This week", "This month", "Earlier"];

/**
 * Sidebar tabs. Globe-key dictations are short, throwaway, and pile up fast, so
 * they get their own lane; everything longer — recordings and dropped files —
 * reads as a "note". `match` decides which sources land in each tab.
 */
type TabKey = "all" | "dictation" | "notes";
const TABS: { key: TabKey; label: string; match: (s: Origin) => boolean }[] = [
  { key: "all", label: "ALL", match: () => true },
  { key: "dictation", label: "DICTATION", match: (s) => s === "hotkey" },
  { key: "notes", label: "NOTES", match: (s) => s !== "hotkey" },
];

/**
 * The version, and the only thing in the app that reaches the network.
 *
 * Click once to ask GitHub whether there's a newer release; click again, when
 * there is, to open its page. Nothing checks on its own — a transcription app
 * that phones home on launch is a different product, and the version sitting
 * quietly in the footer is worth more than a badge nagging about updates.
 *
 * The result is deliberately short-lived: after a check that finds nothing, the
 * strip goes back to the version rather than keeping a green tick around. A
 * permanent "up to date" is a claim about the future.
 */
function Version() {
  const [version, setVersion] = useState<string | null>(null);
  const [state, setState] = useState<
    { at: "idle" } | { at: "checking" } | { at: "current" } | { at: "new"; version: string } | { at: "failed"; why: string }
  >({ at: "idle" });

  useEffect(() => {
    let cancelled = false;
    appVersion()
      .then((v) => !cancelled && setVersion(v))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // "Nothing new" and failures both fade back to the version. An error that
  // stays on screen forever reads as a broken app rather than a missed request.
  useEffect(() => {
    if (state.at !== "current" && state.at !== "failed") return;
    const t = setTimeout(() => setState({ at: "idle" }), 4000);
    return () => clearTimeout(t);
  }, [state]);

  const click = () => {
    if (state.at === "new") {
      openRelease(state.version).catch(() => {});
      return;
    }
    if (state.at === "checking") return;
    setState({ at: "checking" });
    checkUpdate()
      .then((u) =>
        setState(u.newer ? { at: "new", version: u.latest } : { at: "current" }),
      )
      .catch((e) => setState({ at: "failed", why: String(e) }));
  };

  const label =
    state.at === "checking"
      ? "CHECKING…"
      : state.at === "current"
        ? "UP TO DATE"
        : state.at === "new"
          ? `${state.version} AVAILABLE`
          : state.at === "failed"
            ? "CHECK FAILED"
            : `VOICEDUMP ${version ?? ""}`.trim();

  return (
    <button
      onClick={click}
      title={
        state.at === "new"
          ? `Version ${state.version} has been published. Opens the release page.`
          : state.at === "failed"
            ? state.why
            : "Check GitHub for a newer version. Nothing about you is sent."
      }
      className={`diagnostic transition-colors hover:text-ink ${
        state.at === "new" ? "text-sage-dim" : state.at === "failed" ? "text-amber" : ""
      }`}
    >
      {label}
    </button>
  );
}

export function Sidebar({
  items,
  activeId,
  query,
  onQueryChange,
  onSelect,
  onNew,
  ingest,
  namingIds,
  insightsOpen,
  onInsights,
}: Props) {
  const [tab, setTab] = useState<TabKey>("all");
  const { theme, toggle } = useTheme();

  // Lives in the backend rather than localStorage: the globe key reads it from
  // a thread that has no webview to ask. `null` until the first read lands, so
  // the control doesn't flicker through a wrong state on launch.
  const [livePreview, setLive] = useState<boolean | null>(null);
  useEffect(() => {
    let cancelled = false;
    getSettings()
      .then((s) => {
        if (!cancelled) setLive(s.live_preview);
      })
      .catch(() => {
        if (!cancelled) setLive(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const togglePreview = () => {
    // Until the stored value has landed, `livePreview` is null — and `!null` is
    // true, so a click in that window turned the preview *on* no matter what
    // was on disk, and persisted it. The control is disabled until we know.
    if (livePreview === null) return;
    const next = !livePreview;
    // Optimistic: the write is a small file, and snapping back on failure is
    // clearer than a control that lags a click behind.
    setLive(next);
    setLivePreview(next).catch(() => setLive(!next));
  };

  // Remember each row's last title so we can play the reveal only when a title
  // actually *changes* (an AI rename), not on first paint or when filtering.
  const prevTitles = useRef<Map<string, string>>(new Map());
  useEffect(() => {
    const m = prevTitles.current;
    m.clear();
    for (const it of items) m.set(it.id, it.title);
  });

  // Per-tab counts come off the full list so the numbers stay put as you switch.
  const counts = useMemo(() => {
    const c: Record<TabKey, number> = { all: 0, dictation: 0, notes: 0 };
    for (const item of items)
      for (const t of TABS) if (t.match(item.source)) c[t.key]++;
    return c;
  }, [items]);

  const groups = useMemo(() => {
    const active = TABS.find((t) => t.key === tab) ?? TABS[0];
    const bucketed = new Map<string, TranscriptMeta[]>();
    for (const item of items) {
      if (!active.match(item.source)) continue;
      const key = dateGroup(item.created_at);
      const list = bucketed.get(key);
      if (list) list.push(item);
      else bucketed.set(key, [item]);
    }
    return GROUP_ORDER.filter((g) => bucketed.has(g)).map((g) => ({
      label: g,
      items: bucketed.get(g)!,
    }));
  }, [items, tab]);

  // Rows wipe in from the left edge — like rows printing, not cards floating.
  const scope = useGsap(({ scope }) => {
    gsap.fromTo(
      scope.querySelectorAll("[data-row]"),
      { opacity: 0, x: -6 },
      {
        opacity: 1,
        x: 0,
        duration: 0.28,
        ease: EASE.snap,
        stagger: 0.022,
        overwrite: true,
      },
    );
  }, [tab, items.map((i) => i.id).join(",")]);

  return (
    <aside
      ref={scope}
      className="flex h-full w-[270px] shrink-0 flex-col border-r border-hairline bg-rail"
    >
      {/* §5 Top rail: brand = pixel cluster + wordmark + mono sub-line. */}
      <header className="drag-region titlebar-pad border-b border-hairline px-4 pb-3">
        <div className="flex items-center gap-2 text-ink">
          <PixelCluster pattern={CLUSTERS.brand} size={3.5} animate />
          <span className="text-[14px] font-semibold tracking-[-0.01em]">
            VoiceDumps
          </span>
        </div>
        <p className="eyebrow mt-1 text-faint">LOCAL TRANSCRIPTION // WHISPER MED</p>
      </header>

      <div className="border-b border-hairline px-4 py-3">
        <button
          onClick={onNew}
          data-new
          className="group flex w-full items-center gap-2 border border-ink bg-ink px-3 py-2 text-left transition-colors hover:bg-transparent hover:text-ink"
        >
          <span className="text-surface transition-colors group-hover:text-ink">
            <PixelCluster pattern={CLUSTERS.file} size={3} />
          </span>
          <span className="micro text-surface transition-colors group-hover:text-ink">
            NEW TRANSCRIPTION
          </span>
        </button>

        {/* Insights sits with the actions rather than in the list: it describes
            the whole history, so it isn't one more row to scroll past. */}
        <button
          onClick={onInsights}
          aria-pressed={insightsOpen}
          className={`mt-2 flex w-full items-center gap-2 border px-3 py-2 text-left transition-colors ${
            insightsOpen
              ? "border-sage-dim bg-sage-dim/15 text-ink"
              : "border-hairline text-grey hover:border-sage-dim hover:text-ink"
          }`}
        >
          <PixelCluster pattern={CLUSTERS.search} size={3} />
          <span className="micro">INSIGHTS</span>
        </button>

        <label className="mt-3 flex items-center gap-2 border border-hairline bg-panel px-2.5 py-1.5 focus-within:border-sage-dim">
          <span className="text-faint">
            <PixelCluster pattern={CLUSTERS.search} size={2.5} />
          </span>
          <input
            value={query}
            onChange={(e) => onQueryChange(e.target.value)}
            placeholder="SEARCH"
            spellCheck={false}
            className="micro selectable w-full bg-transparent text-ink outline-none placeholder:text-faint"
          />
        </label>
      </div>

      {/* Tabs: separate the pile of quick globe-key dictations from real notes.
          Purely a client-side filter over the already-loaded list. */}
      <div className="flex items-stretch border-b border-hairline">
        {TABS.map((t) => {
          const on = t.key === tab;
          return (
            <button
              key={t.key}
              onClick={() => setTab(t.key)}
              className={[
                "micro flex flex-1 items-center justify-center gap-1.5 border-r border-hairline py-2 transition-colors last:border-r-0",
                on
                  ? "bg-ink text-surface"
                  : "text-faint hover:bg-panel/60 hover:text-grey",
              ].join(" ")}
            >
              <span>{t.label}</span>
              <span className={on ? "text-surface/60" : "text-faint/70"}>
                {counts[t.key]}
              </span>
            </button>
          );
        })}
      </div>

      {/* Live row for work arriving from outside the window. Sits above the
          groups because it's about to become the newest entry. */}
      {ingest && (
        <div className="border-b border-hairline bg-panel px-4 py-2.5">
          <div className="flex items-center gap-1.5">
            <span className="micro shrink-0 border border-sage-dim px-1 text-sage-dim">
              {ORIGIN[ingest.source]?.mark ?? "··"}
            </span>
            <span className="truncate text-[12.5px] leading-tight text-ink">
              {ingest.title}
            </span>
          </div>
          <p className="mono-data mt-1 truncate text-[9px] uppercase tracking-[0.1em] text-faint">
            {ingest.stage || "WORKING"}
          </p>
          <div className="mt-1.5 h-[3px] w-full bg-hairline-soft">
            <div
              className="h-full bg-sage-dim transition-[width] duration-300"
              style={{ width: `${Math.max(2, ingest.progress * 100)}%` }}
            />
          </div>
        </div>
      )}

      <nav className="scroll-slim flex-1 overflow-y-auto">
        {groups.length === 0 ? (
          <p className="micro px-4 py-8 text-center leading-relaxed text-faint">
            {query ? "NO MATCH" : tab === "all" ? "NOTHING YET" : "NONE HERE"}
          </p>
        ) : (
          groups.map((group) => (
            <section key={group.label}>
              {/* §5 Group frame: a solid forest header tab sits flush on top. */}
              <h2 className="micro sticky top-0 z-10 flex items-center justify-between bg-ink px-4 py-1.5 text-surface">
                <span>{group.label}</span>
                <span className="text-surface/60">
                  {String(group.items.length).padStart(2, "0")}
                </span>
              </h2>

              <ul>
                {group.items.map((item) => {
                  const active = item.id === activeId;
                  const naming = namingIds.has(item.id);
                  const prev = prevTitles.current.get(item.id);
                  const renamed = prev !== undefined && prev !== item.title;
                  return (
                    <li key={item.id} data-row className="gsap-init">
                      <button
                        onClick={() => onSelect(item.id)}
                        className={[
                          "relative w-full border-b border-hairline-soft px-4 py-2.5 text-left transition-colors",
                          active ? "bg-panel" : "hover:bg-panel/60",
                        ].join(" ")}
                      >
                        {/* Rung marker: a solid bar flags the active row. */}
                        <span
                          className={[
                            "absolute left-0 top-0 h-full w-[3px] transition-colors",
                            active ? "bg-ink" : "bg-transparent",
                          ].join(" ")}
                        />
                        {/* On a rename, the key flips so the span remounts and
                            replays the reveal; otherwise the key is stable so
                            it never animates on paint or filtering. */}
                        <span
                          key={renamed ? `named:${item.title}` : "title"}
                          className={[
                            "block truncate text-[12.5px] leading-tight",
                            renamed ? "title-reveal" : "",
                            active ? "font-semibold text-ink" : "text-grey",
                          ].join(" ")}
                        >
                          {item.title}
                        </span>
                        {naming ? (
                          <span className="mono-data mt-1 flex items-center gap-1.5 text-[9px] uppercase tracking-[0.1em] text-sage-dim">
                            <span className="animate-pulse">
                              <PixelCluster
                                pattern={CLUSTERS.brand}
                                size={2}
                                pulse
                              />
                            </span>
                            <span>NAMING…</span>
                          </span>
                        ) : (
                          <span className="mono-data mt-1 flex items-center gap-1.5 text-[9px] uppercase tracking-[0.1em] text-faint">
                            <span
                              title={ORIGIN[item.source]?.label}
                              className={
                                item.source === "file"
                                  ? "text-faint"
                                  : "text-sage-dim"
                              }
                            >
                              {ORIGIN[item.source]?.mark ?? "FL"}
                            </span>
                            <span>//</span>
                            <span>{formatRelativeDate(item.created_at)}</span>
                            <span>//</span>
                            <span>{formatDuration(item.duration)}</span>
                          </span>
                        )}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </section>
          ))
        )}
      </nav>

      {/* Live preview: the draft transcript that appears in the overlay while
          you are still speaking. Its own row rather than a chip in the strip
          below, because unlike the theme it is not a readout of anything — it
          changes what dictation does, and it needs room to say so. */}
      <button
        onClick={togglePreview}
        disabled={livePreview === null}
        aria-pressed={livePreview === true}
        title={
          livePreview
            ? "The overlay drafts your words as you speak, using the fast model. It is a rough guess, and the final text is transcribed again from the whole recording."
            : "Show a rough draft in the overlay while you speak. It uses the fast model, so it makes mistakes the pasted text will not."
        }
        className="group flex items-center justify-between gap-2 border-t border-hairline px-4 py-2 text-left transition-colors hover:bg-panel"
      >
        <span className="diagnostic transition-colors group-hover:text-ink">
          LIVE PREVIEW
        </span>
        <span className="flex items-center gap-1.5">
          {/* Filled when on, hollow when off — same swatch vocabulary as the
              theme control directly beneath it. */}
          <span
            className={[
              "h-[7px] w-[7px] border",
              livePreview ? "border-ink bg-ink" : "border-hairline bg-transparent",
            ].join(" ")}
          />
          <span
            className={`diagnostic ${livePreview ? "text-ink" : "text-faint"}`}
          >
            {livePreview === null ? "—" : livePreview ? "ON" : "OFF"}
          </span>
        </span>
      </button>

      {/* §5 Footer diagnostic strip. The theme sits here rather than behind a
          settings pane: it's a readout of the app's current state, which is
          exactly what this strip is for. */}
      <footer className="flex items-center justify-between gap-2 border-t border-hairline px-4 py-2">
        <Version />
        <div className="flex items-center gap-3">
          <button
            onClick={toggle}
            title={`Switch to ${theme === "dark" ? "light" : "dark"} mode`}
            aria-label={`Theme: ${theme}. Switch to ${theme === "dark" ? "light" : "dark"} mode.`}
            className="diagnostic flex items-center gap-1.5 transition-colors hover:text-ink"
          >
            {/* Two squares, filled with the actual surfaces — the swatch shows
                what each mode is, the outline shows which one you're in. */}
            <span
              className={[
                "h-[7px] w-[7px] border",
                theme === "light"
                  ? "border-ink bg-paper"
                  : "border-hairline bg-paper/40",
              ].join(" ")}
            />
            <span
              className={[
                "h-[7px] w-[7px] border",
                theme === "dark"
                  ? "border-ink bg-forest"
                  : "border-hairline bg-forest/40",
              ].join(" ")}
            />
            <span>{theme === "dark" ? "DARK" : "LIGHT"}</span>
          </button>
          <span className="diagnostic mono-data">
            {String(items.length).padStart(3, "0")} REC
          </span>
        </div>
      </footer>
    </aside>
  );
}
