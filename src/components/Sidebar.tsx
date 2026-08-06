import { useEffect, useMemo, useRef, useState } from "react";
import gsap from "gsap";

import type {
  IngestProgress,
  Origin,
  Settings,
  TranscriptMeta,
} from "../lib/api";
import { appVersion, checkUpdate, getTranscript, openRelease } from "../lib/api";
import { dateGroup, formatDuration, formatRelativeDate } from "../lib/format";
import { glyphs, words } from "../lib/shortcut";
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
  askOpen: boolean;
  onAsk: () => void;
  /** Null until the backend's copy lands; the row reads "—" until then. */
  settings: Settings | null;
  settingsOpen: boolean;
  onSettings: () => void;
};

/**
 * Origin marks. Two letters in mono rather than an icon: at 9px a glyph is
 * mush, and these sit next to the existing mono metadata line so they read as
 * part of the same readout.
 */
const ORIGIN: Record<Origin, { mark: string; label: string }> = {
  file: { mark: "FL", label: "Dropped file" },
  mic: { mark: "MIC", label: "Recorded in app" },
  discord: { mark: "DC", label: "From Discord" },
  hotkey: { mark: "FN", label: "Dictated with the globe key" },
  meeting: { mark: "MTG", label: "Recorded meeting" },
};

const GROUP_ORDER = ["Today", "Yesterday", "This week", "This month", "Earlier"];

/**
 * Sidebar tabs. Globe-key dictations are short, throwaway, and pile up fast, so
 * they get their own lane; everything longer (recordings, dropped files,
 * Discord) reads as a "note". `match` decides which sources land in each tab.
 *
 * Two of the four are labelled with the origin mark their own rows carry rather
 * than with the word: the dictation lane is `FN` all the way down and the
 * meetings lane is `MTG`, so the tab is saying what is under it in the same
 * two or three letters. It reads as a legend for the column instead of a
 * heading over it, and it is what makes four lanes fit a 270px rail — the words
 * did not, and an icon would not either, for the reason written above `ORIGIN`.
 *
 * `ALL` and `NOTES` keep their words because neither is one origin: `ALL` is
 * every mark at once and `NOTES` is whatever is left after the other two, so
 * there is no badge that would be telling the truth. Both are short anyway,
 * which is why the crowding was never coming from them.
 */
type TabKey = "all" | "dictation" | "meetings" | "notes";
const TABS: {
  key: TabKey;
  label: string;
  /** Spelled out on hover, since a mark is only obvious once you have scrolled. */
  title: string;
  match: (s: Origin) => boolean;
}[] = [
  { key: "all", label: "ALL", title: "Everything", match: () => true },
  {
    key: "dictation",
    label: ORIGIN.hotkey.mark,
    title: ORIGIN.hotkey.label,
    match: (s) => s === "hotkey",
  },
  {
    key: "meetings",
    label: ORIGIN.meeting.mark,
    title: ORIGIN.meeting.label,
    match: (s) => s === "meeting",
  },
  // Meetings have their own lane now, so they leave this one. What is left is
  // what "note" always meant here: something you brought in or recorded alone.
  {
    key: "notes",
    label: "NOTES",
    title: "Dropped files and recordings",
    match: (s) => s !== "hotkey" && s !== "meeting",
  },
];

const SHEET = [true, true, true, true, true, true, true, true, true];
/**
 * The sheet behind: its top row and right column only — the rest sits under the
 * front one. Drawing the whole square there instead gives a lumpy blob; this
 * gives an edge, which is what actually says "there is another one of these".
 */
const SHEET_BEHIND = [true, true, true, false, false, true, false, false, true];

/**
 * Copy, in the app's own vocabulary: two square sheets, one behind the other.
 *
 * §4.4 replaces stroke icons with pixel clusters, so this is assembled out of
 * two of them rather than borrowed from an icon set — the sidebar has no other
 * drawn glyph in it and one copy icon is not worth becoming the exception.
 */
function CopyMark() {
  return (
    // `flex` on the two wrappers rather than the default inline flow: an inline
    // child sits on a line box, and the strut under it would push the front
    // sheet off its corner by a few pixels.
    <span aria-hidden className="relative block h-[11px] w-[11px]">
      <span className="absolute right-0 top-0 flex opacity-45">
        <PixelCluster pattern={SHEET_BEHIND} size={2} gap={0.7} />
      </span>
      <span className="absolute bottom-0 left-0 flex">
        <PixelCluster pattern={SHEET} size={2} gap={0.7} />
      </span>
    </span>
  );
}

/**
 * Copy a note's text without opening it, from the row it sits on.
 *
 * The reading view keeps its own COPY button — this is for the case that button
 * is wrong for: you know which note you want, you only want what is in it, and
 * opening it first is two clicks and a repaint you didn't need.
 *
 * The text is fetched on the click rather than carried by the row. The sidebar
 * is given metadata only, and holding every transcript in memory to service a
 * button most rows never show would be a poor trade for a library of hundreds.
 */
function QuickCopy({ id }: { id: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");

  // Back to COPY on its own. A tick that stays put stops being feedback about
  // this click and starts looking like a property of the row.
  useEffect(() => {
    if (state === "idle") return;
    const t = setTimeout(() => setState("idle"), 1600);
    return () => clearTimeout(t);
  }, [state]);

  const copy = async (e: React.MouseEvent) => {
    // Copying is not selecting: the row behind this is a button of its own.
    e.stopPropagation();
    try {
      const { text } = await getTranscript(id);
      await navigator.clipboard.writeText(text);
      setState("copied");
    } catch {
      setState("failed");
    }
  };

  const said =
    state === "copied"
      ? "Copied"
      : state === "failed"
        ? "Could not copy this transcript"
        : "Copy this transcript";

  return (
    <button
      onClick={copy}
      aria-label={said}
      title={said}
      className={[
        "absolute right-2 top-1/2 flex h-[22px] w-[22px] -translate-y-1/2",
        "items-center justify-center border",
        // Hidden until the row is under the cursor, but reachable by keyboard:
        // an action that only exists on hover doesn't exist for half the people
        // who might use it.
        "opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100",
        state === "copied"
          ? "border-sage-dim bg-panel text-sage-dim"
          : state === "failed"
            ? "border-amber bg-panel text-amber"
            : "border-hairline bg-panel text-grey hover:border-sage-dim hover:text-ink",
      ].join(" ")}
    >
      {state === "copied" ? (
        <PixelCluster pattern={CLUSTERS.done} size={2.5} gap={1} />
      ) : state === "failed" ? (
        <PixelCluster pattern={CLUSTERS.warn} size={2.5} gap={1} />
      ) : (
        <CopyMark />
      )}
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
  askOpen,
  onAsk,
  settings,
  settingsOpen,
  onSettings,
}: Props) {
  const [tab, setTab] = useState<TabKey>("all");
  const { theme, toggle } = useTheme();

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
    const c: Record<TabKey, number> = { all: 0, dictation: 0, meetings: 0, notes: 0 };
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

        {/* Ask sits above search on purpose. Search finds the note that
            contains a word; this answers the question you actually had, and
            when you know which note you want, search is still right there. */}
        <button
          onClick={onAsk}
          aria-pressed={askOpen}
          className={`mt-2 flex w-full items-center gap-2 border px-3 py-2 text-left transition-colors ${
            askOpen
              ? "border-sage-dim bg-sage-dim/15 text-ink"
              : "border-hairline text-grey hover:border-sage-dim hover:text-ink"
          }`}
        >
          <PixelCluster pattern={CLUSTERS.brand} size={3} />
          <span className="micro">ASK YOUR NOTES</span>
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
        {/* MTG earns its place rather than holding it: someone who has never
            recorded a call should not pay for the lane in width. It appears
            with the first meeting and stays. */}
        {TABS.filter((t) => t.key !== "meetings" || counts.meetings > 0).map((t) => {
          const on = t.key === tab;
          return (
            <button
              key={t.key}
              onClick={() => setTab(t.key)}
              title={t.title}
              className={[
                "micro flex flex-1 items-center justify-center gap-1.5 border-r border-hairline px-1 py-2 transition-colors last:border-r-0",
                on
                  ? "bg-ink text-surface"
                  : "text-faint hover:bg-panel/60 hover:text-grey",
              ].join(" ")}
            >
              <span>{t.label}</span>
              {/* Tabular figures so the count does not resize the lane as it
                  crosses 9 to 10 to 100 — four lanes sharing a rail this narrow
                  visibly shuffle when one of them breathes. */}
              <span
                className={[
                  "tabular-nums",
                  on ? "text-surface/60" : "text-faint/70",
                ].join(" ")}
              >
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
                    <li
                      key={item.id}
                      data-row
                      className="group/row relative gsap-init"
                    >
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
                      <QuickCopy id={item.id} />
                    </li>
                  );
                })}
              </ul>
            </section>
          ))
        )}
      </nav>

      {/* One row, however many settings there turn out to be.
          Microphone and live preview each used to own a row here, and every
          new option would have cost another — so they moved into a pane and
          this is the way in. The readout is the dictation chord, because that
          is the one setting worth seeing without opening anything. */}
      <button
        onClick={onSettings}
        aria-pressed={settingsOpen}
        title={
          settings
            ? `Hold ${words(settings.shortcut)} to dictate. Click to change this and more.`
            : "Microphone, live preview and the dictation shortcut"
        }
        className={`group flex items-center justify-between gap-2 border-t border-hairline px-4 py-2 text-left transition-colors ${
          settingsOpen ? "bg-panel" : "hover:bg-panel"
        }`}
      >
        <span
          className={`diagnostic transition-colors group-hover:text-ink ${
            settingsOpen ? "text-ink!" : ""
          }`}
        >
          SETTINGS
        </span>
        {/* The keys as they are printed on the keyboard — 🌐, or ⌃⌥. */}
        <span className="text-[11px] leading-none text-grey">
          {settings ? glyphs(settings.shortcut) : "—"}
        </span>
      </button>

      {/* §5 Footer diagnostic strip. The theme sits here rather than behind a
          settings pane: it's a readout of the app's current state, which is
          exactly what this strip is for. */}
      <footer className="flex items-center justify-between gap-2 border-t border-hairline px-4 py-2">
        <VersionButton />
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

/**
 * The version, which is also the update button.
 *
 * Three jobs in one control, because they are the same question asked at
 * different moments: what am I running, is there anything newer, and take me
 * to it. A separate "check for updates" item in a menu is the version of this
 * that nobody ever clicks.
 *
 * **The daily check.** Someone who never clicks the version should still learn
 * that a release exists, so this asks once a day on its own. Once a *day*, not
 * once a launch: this app gets opened and closed all day long, and a check per
 * launch would be a request every few minutes from anyone using it properly —
 * indistinguishable from telemetry in a packet capture, which is exactly the
 * accusation an app like this cannot afford. The last check is remembered in
 * localStorage so quitting does not reset the clock.
 *
 * **What it does not do.** It does not install anything. See `update.rs` for
 * why: swapping a running application for a binary fetched over HTTPS, trusted
 * because the connection was encrypted, turns a local transcription app into an
 * execution service for whoever holds the domain. Installing needs a signing
 * key that never touches this repository, and until that exists this opens the
 * release page and lets the user decide.
 */
function VersionButton() {
  const [version, setVersion] = useState<string | null>(null);
  const [state, setState] = useState<
    | { at: "idle" }
    | { at: "checking" }
    | { at: "current" }
    | { at: "new"; version: string }
    | { at: "failed"; why: string }
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

  // Where the last automatic check is remembered. Not a setting: there is
  // nothing here for a user to configure, and a preference row for it would
  // imply the app does more in the background than it does.
  const LOOKED_AT = "voicedumps.update.lastCheck";
  const A_DAY = 24 * 60 * 60 * 1000;

  useEffect(() => {
    let cancelled = false;
    let last = 0;
    try {
      last = Number(localStorage.getItem(LOOKED_AT)) || 0;
    } catch {
      // Private browsing, a wiped profile, a string somebody edited. Treat an
      // unreadable clock as "never checked" rather than skipping forever.
    }
    if (Date.now() - last < A_DAY) return;

    // Written before the request, not after: a check that fails should not
    // retry on every mount for the rest of the day.
    try {
      localStorage.setItem(LOOKED_AT, String(Date.now()));
    } catch {
      /* nothing to do — it will simply check again next launch */
    }

    checkUpdate()
      .then((u) => {
        // Silent unless there is something to say. An automatic check that
        // announces "up to date" is a notification about nothing.
        if (!cancelled && u.newer) setState({ at: "new", version: u.latest });
      })
      .catch(() => {
        // No badge, no error. Nobody asked, so nobody is owed a failure.
      });
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
            : `KUPA ${version ?? ""}`.trim();

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
      className={`diagnostic flex items-center gap-1.5 transition-colors hover:text-ink ${
        state.at === "new" ? "text-sage-dim" : state.at === "failed" ? "text-amber" : ""
      }`}
    >
      {/* A dot only when there is something to act on. The strip is a readout,
          and a permanent marker on it would stop meaning anything. */}
      {state.at === "new" && (
        <span className="h-[5px] w-[5px] shrink-0 bg-sage-dim" aria-hidden />
      )}
      <span>{label}</span>
    </button>
  );
}
