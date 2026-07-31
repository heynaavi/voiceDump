import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

import {
  deleteTranscript,
  getTranscript,
  listTranscripts,
  renameTranscript,
  saveTranscript,
  engineHealth,
  startJob,
  updateTranscript,
  watchJob,
  type IngestProgress,
  type JobState,
  type Origin,
  type Paragraph,
  type Transcript,
  type TranscriptMeta,
} from "./lib/api";
import {
  MEDIA_EXTENSION_LIST,
  isSupportedMedia,
  titleFromPath,
} from "./lib/format";

import { DictationPill } from "./components/DictationPill";
import { Insights } from "./components/Insights";
import { DropZone } from "./components/DropZone";
import { JobProgress } from "./components/JobProgress";
import { Sidebar } from "./components/Sidebar";
import { TranscriptView } from "./components/TranscriptView";

export default function App() {
  const [history, setHistory] = useState<TranscriptMeta[]>([]);
  const [query, setQuery] = useState("");
  const [activeId, setActiveId] = useState<string | null>(null);
  const [active, setActive] = useState<Transcript | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [dragging, setDragging] = useState(false);
  const [engineError, setEngineError] = useState<string | null>(null);
  // Work arriving from outside the window — globe-key dictation.
  const [ingest, setIngest] = useState<IngestProgress | null>(null);
  // Transcript ids the AI is currently naming, so cards can show it working.
  const [namingIds, setNamingIds] = useState<Set<string>>(new Set());
  /** Insights replaces the main pane rather than opening a window, so it can be
   *  read next to the list it describes and dismissed by picking any note. */
  const [insights, setInsights] = useState(false);

  // Guards against a stale SSE stream writing over a newer job.
  const jobIdRef = useRef<string | null>(null);

  const refreshHistory = useCallback(async (q: string) => {
    try {
      setHistory(await listTranscripts(q));
    } catch (e) {
      console.error("history load failed", e);
    }
  }, []);

  // -- boot ---------------------------------------------------------------

  useEffect(() => {
    // Just surface engine problems early. The model is deliberately NOT
    // preloaded — it costs ~0.4s to load on demand, which isn't worth holding
    // 1.6 GB for while the app sits open.
    engineHealth()
      .then(({ error }) => {
        if (error) setEngineError(error);
      })
      .catch((e) => setEngineError(String(e)));
  }, []);

  // Background ingest. The Rust side does the work and owns the
  // store, so the UI's job here is purely to reflect it.
  useEffect(() => {
    const subs = [
      listen<IngestProgress>("ingest-progress", (e) => setIngest(e.payload)),
      listen<string>("ingest-done", async (e) => {
        setIngest(null);
        await refreshHistory("");
        setQuery("");
        // Don't yank the user out of whatever they're reading; just surface it
        // in history unless nothing is open.
        setActiveId((prev) => prev ?? e.payload);
      }),
      // A note is being named by the AI (or has finished). Track the id so its
      // card can show a live "naming…" state.
      listen<{ id: string; naming: boolean }>("title-naming", (e) => {
        const { id, naming } = e.payload;
        setNamingIds((s) => {
          const next = new Set(s);
          if (naming) next.add(id);
          else next.delete(id);
          return next;
        });
      }),
      // The AI title landed; swap it in place so the card renames itself
      // (with a reveal animation) without a reload, and clear the naming state.
      listen<{ id: string; title: string }>("title-updated", (e) => {
        const { id, title } = e.payload;
        setNamingIds((s) => {
          const next = new Set(s);
          next.delete(id);
          return next;
        });
        setHistory((h) => h.map((it) => (it.id === id ? { ...it, title } : it)));
        setActive((a) => (a && a.id === id ? { ...a, title } : a));
      }),
    ];
    return () => {
      subs.forEach((s) => s.then((un) => un()).catch(() => {}));
    };
  }, [refreshHistory]);

  // Open the transcript that a background ingest just produced, but only if the
  // user wasn't already reading something.
  useEffect(() => {
    if (!activeId || active?.id === activeId) return;
    getTranscript(activeId)
      .then(setActive)
      .catch((e) => console.error("could not open transcript", e));
  }, [activeId, active?.id]);

  // Initial load plus debounced search.
  useEffect(() => {
    const t = setTimeout(() => refreshHistory(query), query ? 140 : 0);
    return () => clearTimeout(t);
  }, [query, refreshHistory]);

  // -- transcription ------------------------------------------------------

  const runFile = useCallback(
    async (path: string, origin: Origin = "file") => {
      if (!isSupportedMedia(path)) {
        setJob({
          id: "local",
          path,
          status: "error",
          progress: 0,
          stage: "Failed",
          error: "That file type isn't supported. Try an audio or video file.",
        });
        return;
      }

      setActiveId(null);
      setActive(null);

      try {
        const started = await startJob(path);
        jobIdRef.current = started.id;
        setJob(started);

        await watchJob(started.id, async (state) => {
          if (jobIdRef.current !== state.id) return;
          setJob(state);

          if (state.status === "done" && state.result) {
            const id = await saveTranscript({
              title: titleFromPath(state.path),
              sourcePath: state.path,
              duration: state.result.duration,
              language: state.result.language,
              text: state.result.text,
              paragraphs: state.result.paragraphs,
              segments: state.result.segments,
              peaks: state.result.peaks ?? [],
              source: origin,
            });
            jobIdRef.current = null;
            setJob(null);
            await refreshHistory("");
            setQuery("");
            setActiveId(id);
            setActive(await getTranscript(id));
          }
        });
      } catch (e) {
        setJob({
          id: "local",
          path,
          status: "error",
          progress: 0,
          stage: "Failed",
          error: e instanceof Error ? e.message : String(e),
        });
      }
    },
    [refreshHistory],
  );

  // -- drag & drop --------------------------------------------------------

  useEffect(() => {
    let unlisten: Promise<() => void> | null = null;
    try {
      unlisten = getCurrentWebview().onDragDropEvent((event) => {
        if (event.payload.type === "over") {
          setDragging(true);
        } else if (event.payload.type === "drop") {
          setDragging(false);
          const first = event.payload.paths?.[0];
          if (first) runFile(first);
        } else {
          setDragging(false);
        }
      });
    } catch (e) {
      // Losing drag-drop shouldn't take the whole window down — the file
      // picker still works.
      console.error("drag-drop unavailable", e);
    }
    return () => {
      unlisten?.then((f) => f()).catch(() => {});
    };
  }, [runFile]);

  const browse = useCallback(async () => {
    const picked = await open({
      multiple: false,
      filters: [{ name: "Audio & Video", extensions: MEDIA_EXTENSION_LIST }],
    });
    if (typeof picked === "string") runFile(picked);
  }, [runFile]);

  // -- history actions ----------------------------------------------------

  const select = useCallback(async (id: string) => {
    setJob(null);
    setInsights(false);
    setActiveId(id);
    try {
      setActive(await getTranscript(id));
    } catch (e) {
      console.error("could not open transcript", e);
    }
  }, []);

  const rename = useCallback(
    async (id: string, title: string) => {
      await renameTranscript(id, title);
      setActive((prev) => (prev && prev.id === id ? { ...prev, title } : prev));
      refreshHistory(query);
    },
    [refreshHistory, query],
  );

  const remove = useCallback(
    async (id: string) => {
      await deleteTranscript(id);
      if (activeId === id) {
        setActiveId(null);
        setActive(null);
      }
      refreshHistory(query);
    },
    [activeId, refreshHistory, query],
  );

  const editParagraphs = useCallback(
    async (id: string, paragraphs: Paragraph[]) => {
      const text = paragraphs.map((p) => p.text).join("\n\n");
      try {
        await updateTranscript(id, text, paragraphs);
      } catch (e) {
        console.error("could not save edit", e);
        return;
      }
      // Keep the in-memory copy in step so switching away and back doesn't
      // show stale prose, and refresh history for the word count and search.
      setActive((prev) =>
        prev && prev.id === id ? { ...prev, text, paragraphs } : prev,
      );
      refreshHistory(query);
    },
    [refreshHistory, query],
  );

  const newTranscription = useCallback(() => {
    setJob(null);
    setInsights(false);
    setActiveId(null);
    setActive(null);
  }, []);

  // -- render -------------------------------------------------------------

  return (
    <div className="flex h-full bg-surface">
      <Sidebar
        items={history}
        activeId={activeId}
        query={query}
        onQueryChange={setQuery}
        onSelect={select}
        onNew={newTranscription}
        ingest={ingest}
        namingIds={namingIds}
        insightsOpen={insights}
        onInsights={() => setInsights((v) => !v)}
      />

      <main className="relative min-w-0 flex-1">
        {insights ? (
          <Insights />
        ) : job ? (
          <JobProgress job={job} onDismiss={() => setJob(null)} />
        ) : active ? (
          <TranscriptView
            transcript={active}
            onRename={rename}
            onDelete={remove}
            onEdit={editParagraphs}
            naming={namingIds.has(active.id)}
          />
        ) : (
          <DropZone
            dragging={dragging}
            onBrowse={browse}
            onRecorded={(p) => runFile(p, "mic")}
            engineError={engineError}
          />
        )}

        {/* Dictation runs while other apps have focus, so this is both the
            status readout and the only place a permission failure shows up. */}
        <DictationPill />

        {/* A drop anywhere in the window works, even while reading. */}
        {dragging && !job && active && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center bg-surface/85 backdrop-blur-sm">
            <p className="micro border border-sage-dim bg-ink px-5 py-3 text-surface">
              RELEASE TO TRANSCRIBE
            </p>
          </div>
        )}
      </main>
    </div>
  );
}
