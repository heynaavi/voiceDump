import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

import {
  deleteTranscript,
  getSettings,
  getTranscript,
  listTranscripts,
  meetingStart,
  meetingStatus,
  meetingStop,
  chatHistory,
  forgetChat,
  watchDictationSaved,
  watchMeetingDetected,
  watchMeetingEnded,
  watchSideMissing,
  watchMeetingFailed,
  watchMeetingOfferClosed,
  watchMeetingProgress,
  watchMeetingSaved,
  watchMeetingStarted,
  modelsStatus,
  renameSpeaker as apiRenameSpeaker,
  renameTranscript,
  saveTranscript,
  setDiarization,
  setLivePreview,
  setMicrophone,
  setShortcut,
  sidecarStatus,
  startJob,
  updateTranscript,
  watchJob,
  watchCatchingUp,
  type CatchingUp,
  type Detected,
  type IngestProgress,
  type JobState,
  type MeetingCapability,
  type MeetingProgress,
  type ModelStatus,
  type Origin,
  type Paragraph,
  type Settings as Stored,
  type Transcript,
  type TranscriptMeta,
} from "./lib/api";
import {
  MEDIA_EXTENSION_LIST,
  isSupportedMedia,
  titleFromPath,
} from "./lib/format";

import { DictationPill } from "./components/DictationPill";
import { Chat, type Turn } from "./components/Chat";
import { CLUSTERS, PixelCluster } from "./components/PixelCluster";
import { Insights } from "./components/Insights";
import { DropZone } from "./components/DropZone";
import { JobProgress } from "./components/JobProgress";
import { MeetingBar } from "./components/MeetingBar";
import { ModelSetup } from "./components/ModelSetup";
import {
  Onboarding,
  everyChapter,
  unseenChapters,
  type ChapterKey,
} from "./components/Onboarding";
import { Settings } from "./components/Settings";
import { Sidebar } from "./components/Sidebar";
import { TranscriptView } from "./components/TranscriptView";

export default function App() {
  const [history, setHistory] = useState<TranscriptMeta[]>([]);
  const [query, setQuery] = useState("");
  const [activeId, setActiveId] = useState<string | null>(null);
  const [active, setActive] = useState<Transcript | null>(null);
  const [job, setJob] = useState<JobState | null>(null);
  const [dragging, setDragging] = useState(false);
  const [assistantError, setAssistantError] = useState<string | null>(null);
  // Work arriving from outside the window (Discord today, hotkey later).
  const [ingest, setIngest] = useState<IngestProgress | null>(null);
  // Transcript ids the AI is currently naming, so cards can show it working.
  const [namingIds, setNamingIds] = useState<Set<string>>(new Set());
  /** Insights replaces the main pane rather than opening a window, so it can be
   *  read next to the list it describes and dismissed by picking any note. */
  /**
   * Which full-pane view is showing, or none.
   *
   * One value rather than a boolean each, because they were never independent:
   * only one can be on screen, and every place that opens a note had to
   * remember to switch off all of the others. Adding Ask as a fourth meant
   * three of those places silently kept showing the chat when you clicked a
   * transcript — a bug that could only exist while "which pane" was spread
   * across several variables that could disagree.
   */
  const [pane, setPane] = useState<"chat" | "insights" | "settings" | null>(null);
  /**
   * The app working through AI it owes an existing library.
   *
   * Somebody upgrading from a version before titles, overviews and the graph
   * finds hundreds of model calls happening quietly for the next hour. Unsaid,
   * that reads as a Mac gone wrong; said, it reads as catching up.
   */
  const [behind, setBehind] = useState<CatchingUp | null>(null);

  useEffect(
    () =>
      watchCatchingUp((c) =>
        // `doing` empty, or nothing left, means that pass is finished. The
        // strip disappears rather than sitting at zero: a readout that stays
        // put after the work is done is furniture.
        setBehind(c.doing && c.left > 0 ? c : null),
      ),
    [],
  );
  /** The conversation, held here so opening a cited note doesn't end it. */
  const [chat, setChat] = useState<Turn[]>([]);

  // Read back from disk on boot. The conversation is written down as each turn
  // lands, so what comes back here is every question ever asked — including the
  // ones whose window was closed before it could report anything.
  useEffect(() => {
    chatHistory().then(
      (kept) =>
        setChat(
          kept.map((t) => ({
            id: t.id,
            question: t.question,
            answer: t.answer,
            error: t.error || null,
          })),
        ),
      () => {},
    );
  }, []);
  /** Settings does the same. Both are panes, not modals: nothing here is a
   *  decision that has to be finished before the app can be used again. */
  /** The backend's copy, held here rather than in the pane so the sidebar's
   *  readout and the controls can never disagree. Null until the first read. */
  const [settings, setSettings] = useState<Stored | null>(null);
  /** Whether the speech models are on disk. Null until asked; a `ready: false`
   *  answer takes over the whole window, because there is no useful app
   *  underneath it until the weights exist. */
  const [models, setModels] = useState<ModelStatus | null>(null);
  /** Whether this Mac can record a call, asked once at boot. */
  const [meeting, setMeeting] = useState<MeetingCapability | null>(null);
  /** Where an in-flight meeting is: capturing, or transcribing both sides. */
  const [meetingPhase, setMeetingPhase] = useState<
    "recording" | "finishing" | null
  >(null);
  const [meetingStartedAt, setMeetingStartedAt] = useState<number | null>(null);
  /** How far the two transcriptions have got, once a meeting is stopping. */
  const [meetingProgress, setMeetingProgress] = useState<MeetingProgress | null>(
    null,
  );
  /** A refused start or a failed save, shown where the button was. */
  const [meetingError, setMeetingError] = useState<string | null>(null);
  /** The first-run walkthrough. Read once at mount — a value that changed
   *  underneath would reopen the tour over a working app. */
  /**
   * Which chapters of the walkthrough to show, or null for none.
   *
   * Not a boolean any more. A first run gets all of them; somebody upgrading
   * gets only the chapters that did not exist last time they looked, so a new
   * feature can introduce itself without re-teaching four they already know.
   * Replaying from Settings passes every chapter regardless.
   */
  const [tutorial, setTutorial] = useState<ChapterKey[] | null>(() => {
    const unseen = unseenChapters();
    return unseen.length ? unseen : null;
  });
  /** An app has the microphone open and we have offered to take notes. */
  const [detected, setDetected] = useState<Detected | null>(null);

  // Guards against a stale SSE stream writing over a newer job.
  const jobIdRef = useRef<string | null>(null);

  const refreshHistory = useCallback(async (q: string) => {
    try {
      setHistory(await listTranscripts(q));
    } catch (e) {
      console.error("history load failed", e);
    }
  }, []);

  /** Ask what the backend makes of itself, and say so either way.
   *
   * Assigning the answer rather than only assigning failures matters in the
   * lite build, where this command reports whether the speech model is on
   * disk. On a first run it is asked before the models have downloaded, so it
   * correctly says they are missing — and if that verdict could never be
   * withdrawn the banner would sit there for the rest of the session over a
   * working app. It is re-asked the moment setup finishes. */
  const checkAssistant = useCallback(() => {
    sidecarStatus()
      .then(({ error }) => setAssistantError(error))
      .catch((e) => setAssistantError(String(e)));
  }, []);

  // -- boot ---------------------------------------------------------------

  useEffect(() => {
    // Surface a missing AI layer early, so the first note that comes back with
    // a timestamp title instead of a real one isn't a mystery. Transcription
    // doesn't go through here at all — that's the in-process Rust engine.
    checkAssistant();

    // The dictation key reads these from a thread with no webview to ask, so
    // the backend owns them and the window is only ever showing a copy.
    getSettings()
      .then(setSettings)
      .catch((e) => console.error("could not read settings", e));

    // Are the weights here? Almost always yes, and then this costs two `stat`
    // calls. On a first run it is the difference between a setup screen and a
    // dictation key that appears to do nothing. If the question itself fails,
    // assume ready: a broken check must not lock a working install behind a
    // download screen.
    modelsStatus()
      .then(setModels)
      .catch((e) => {
        console.error("could not check for the speech models", e);
        setModels({ ready: true, needed: [], bytes: 0 });
      });

    // Can this Mac hear the far side of a call? Answered once — the macOS
    // version and the bundled helper are both fixed for the life of the
    // process. A recording already in flight is picked up too, so a window
    // reload during a meeting comes back to a live bar rather than losing it.
    meetingStatus()
      .then((capability) => {
        setMeeting(capability);
        if (capability.recording) setMeetingPhase("recording");
      })
      .catch((e) => {
        // Treat an unanswerable question as "no", not as a broken app: every
        // other way in still works, and offering a button that cannot start is
        // worse than not offering one.
        console.error("could not check meeting capture", e);
        setMeeting({ available: false, reason: "", recording: false });
      });
  }, [checkAssistant]);

  // -- meetings -----------------------------------------------------------

  const startMeeting = useCallback(async () => {
    setMeetingError(null);
    setDetected(null);
    try {
      // The phase is not set here. A meeting can also be started from the
      // floating card, which this window never hears about directly, so the
      // backend's `meeting-started` is the single thing that moves the UI —
      // and it fires for both doors.
      await meetingStart();
    } catch (e) {
      setMeetingError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  // The backend does the noticing — it has to keep watching while this window
  // is behind a browser — so all that happens here is showing the offer.
  useEffect(() => {
    const subs = [
      watchMeetingDetected(setDetected),
      // The app let go of the microphone. Whatever the offer was about is over,
      // so it should not still be sitting there afterwards.
      watchMeetingEnded((bundle) =>
        setDetected((d) => (d?.bundle === bundle ? null : d)),
      ),
      // The floating offer answers for both surfaces: it is the one with the
      // countdown, so when it goes, this goes.
      watchMeetingOfferClosed(() => setDetected(null)),
    ];
    return () => {
      subs.forEach((s) => s.then((un) => un()).catch(() => {}));
    };
  }, []);

  const stopMeeting = useCallback(async () => {
    setMeetingPhase("finishing");
    try {
      await meetingStop();
    } catch (e) {
      setMeetingPhase(null);
      setMeetingStartedAt(null);
      setMeetingProgress(null);
      setMeetingError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  // How a meeting ends, wherever it was stopped from. The window used to own
  // this by awaiting its own stop call, which quietly meant a meeting stopped
  // from the floating card finished with the UI still showing it recording.
  useEffect(() => {
    const subs = [
      watchMeetingStarted((startedMs) => {
        setMeetingError(null);
        setDetected(null);
        setMeetingStartedAt(startedMs);
        setMeetingProgress(null);
        setMeetingPhase("recording");
      }),
      // The clock stops here, not in `stopMeeting`.
      //
      // A meeting can be stopped from the floating card, which this window
      // never hears about — so ending one that way used to leave the bar
      // reading RECORDING with the elapsed time still climbing for the whole
      // transcription. On a fifty-minute call that is fifteen minutes of the
      // app insisting it is still listening while it is in fact writing the
      // note. The first progress report is the honest signal that capture is
      // over, wherever the stop came from, and it is subscribed for the life of
      // the window rather than from the moment the phase changes — the
      // subscription is a promise, and the "Stopping" report does not wait for
      // it to resolve.
      watchMeetingProgress((p) => {
        setMeetingProgress(p);
        setMeetingPhase((phase) => (phase === "recording" ? "finishing" : phase));
      }),
      watchMeetingSaved(async (id) => {
        setMeetingPhase(null);
        setMeetingStartedAt(null);
        setMeetingProgress(null);
        setPane(null);
        await refreshHistory("");
        setQuery("");
        setActiveId(id);
        try {
          setActive(await getTranscript(id));
        } catch (e) {
          console.error("could not open the meeting", e);
        }
      }),
      // A silent meeting lands here, and it is the failure someone is most
      // likely to cause themselves — so it is said out loud, not swallowed.
      watchMeetingFailed((reason) => {
        setMeetingPhase(null);
        setMeetingStartedAt(null);
        setMeetingProgress(null);
        setMeetingError(reason);
      }),
      // Not a failure — the meeting keeps recording your side — so the phase is
      // deliberately left alone and only the message changes. Said at all
      // because the alternative is what actually happened once: seventy-six
      // minutes recorded, one side captured, and nothing on screen about it
      // until the transcript came back two words long.
      watchSideMissing(setMeetingError),
    ];
    return () => {
      subs.forEach((s) => s.then((un) => un()).catch(() => {}));
    };
  }, [refreshHistory]);

  // Each writer is optimistic — the store is a small JSON file, and snapping
  // back on failure reads better than a control that lags a click behind. The
  // backend answers with the whole settings object, so its reply is the truth.
  const applyLivePreview = useCallback((enabled: boolean) => {
    setSettings((s) => (s ? { ...s, live_preview: enabled } : s));
    setLivePreview(enabled).then(setSettings).catch(() => {
      setSettings((s) => (s ? { ...s, live_preview: !enabled } : s));
    });
  }, []);

  const applyDiarization = useCallback((enabled: boolean) => {
    // Optimistic, then reconciled — same as the preview switch above. A toggle
    // that waits on the disk feels broken even when it works.
    setSettings((s) => (s ? { ...s, diarization: enabled } : s));
    setDiarization(enabled).then(setSettings).catch(() => {
      setSettings((s) => (s ? { ...s, diarization: !enabled } : s));
    });
  }, []);

  const applyMicrophone = useCallback((name: string | null) => {
    setSettings((s) => (s ? { ...s, microphone: name } : s));
    setMicrophone(name)
      .then(setSettings)
      .catch(() => getSettings().then(setSettings).catch(() => {}));
  }, []);

  // Not optimistic, and it returns its promise: a refused chord has to surface
  // as the recorder's own error rather than as a control that silently reverts.
  const applyShortcut = useCallback(async (chord: string) => {
    setSettings(await setShortcut(chord));
  }, []);

  // Background ingest (Discord). The Rust side does the work and owns the
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
              model: state.result.model,
              transcribeMs: state.result.transcribe_ms,
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
    setPane(null);
    setActiveId(id);
    try {
      setActive(await getTranscript(id));
    } catch (e) {
      console.error("could not open transcript", e);
    }
  }, []);

  /** Open the newest note in the library, once, at launch.
   *
   * The app opened on the drop zone with nothing selected, which is the right
   * first-run screen and the wrong every-other-run one — what you want is
   * almost always the thing you recorded last. Guarded by a ref rather than by
   * the list being empty, so it fires exactly once: a later refresh (a search,
   * a delete, an AI rename) must never drag the reader back to the top.
   *
   * Deliberately gives way to anything already on screen. A meeting that
   * finished while the window was booting, or a file mid-transcription, is more
   * recent news than the library is. */
  const opened = useRef(false);
  useEffect(() => {
    if (opened.current || history.length === 0) return;
    opened.current = true;
    if (activeId || job) return;
    select(history[0].id);
  }, [history, activeId, job, select]);

  // A dictation, the moment it saves. Unlike a Discord message arriving in the
  // background, this is something the user did on purpose a few seconds ago, so
  // opening it is not a yank — and it lands with a fallback title that the AI
  // renames a moment later, which is worth being on screen for.
  useEffect(() => {
    const sub = watchDictationSaved(async (id) => {
      await refreshHistory("");
      setQuery("");
      select(id);
    });
    return () => {
      sub.then((un) => un()).catch(() => {});
    };
  }, [refreshHistory, select]);

  const rename = useCallback(
    async (id: string, title: string) => {
      await renameTranscript(id, title);
      setActive((prev) => (prev && prev.id === id ? { ...prev, title } : prev));
      refreshHistory(query);
    },
    [refreshHistory, query],
  );

  /** Name one side of a meeting. The backend answers with the rewritten
   *  transcript — every turn, every segment, and the overview's action-item
   *  owners — so there is nothing to refetch. History is still refreshed: the
   *  full-text index covers the transcript, and searching for a name you have
   *  just given somebody should find the meeting you gave it in. */
  const renameSpeaker = useCallback(
    async (id: string, from: string, to: string) => {
      const updated = await apiRenameSpeaker(id, from, to);
      setActive((prev) => (prev && prev.id === id ? updated : prev));
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
    setPane(null);
    setActiveId(null);
    setActive(null);
  }, []);

  // -- render -------------------------------------------------------------

  // First run, before anything else can be drawn. Deliberately after every
  // hook above so the hook order is identical on both sides of the gate, and
  // deliberately not while `models` is still null — flashing a download screen
  // at someone who has the models would be worse than a beat of nothing.
  if (models && !models.ready) {
    return (
      <ModelSetup
        status={models}
        onReady={() => {
          setModels({ ready: true, needed: [], bytes: 0 });
          // The boot check ran before these existed and said so. Ask again
          // now that they do, or the app opens behind a stale "model is
          // missing" banner it can no longer take back.
          checkAssistant();
        }}
      />
    );
  }

  // After the models, never before: the walkthrough asks the user to dictate,
  // and there is nothing to dictate with until the weights are on disk.
  if (tutorial) {
    return (
      <Onboarding
        settings={settings}
        meeting={meeting}
        chapters={tutorial}
        onDone={() => setTutorial(null)}
      />
    );
  }

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
        insightsOpen={pane === "insights"}
        onInsights={() => setPane((p) => (p === "insights" ? null : "insights"))}
        askOpen={pane === "chat"}
        onAsk={() => setPane((p) => (p === "chat" ? null : "chat"))}
        settings={settings}
        settingsOpen={pane === "settings"}
        onSettings={() => setPane((p) => (p === "settings" ? null : "settings"))}
      />

      <main className="relative min-w-0 flex-1">
        {/* Catching up on an older library: naming notes, writing overviews,
            reading what they are about.

            A strip rather than a dialog, and it does not take the pointer —
            everything underneath keeps working while this runs, which is the
            whole point. It is one line because there is no honest total to
            draw a bar from: the graph pass is capped per launch and a failed
            note comes back next time, so a bar would go backwards. A number
            counting down is true. */}
        {behind && (
          <div
            aria-live="polite"
            className="pointer-events-none absolute inset-x-0 top-0 z-30 flex justify-center px-4 pt-2"
          >
            <div className="flex items-center gap-2 border border-hairline bg-panel/90 px-2.5 py-1 backdrop-blur">
              <span className="h-[5px] w-[5px] shrink-0 animate-pulse bg-sage-dim" />
              <span className="diagnostic">
                {behind.doing.toUpperCase()} — {behind.left} LEFT
              </span>
            </div>
          </div>
        )}

        {/* The way back into a conversation you stepped out of.
            Floating over the pane rather than living in it, because the thing
            it returns you to is not part of what you are looking at — and it
            only exists while there is something to return to, so it never
            becomes furniture. */}
        {pane !== "chat" && chat.length > 0 && (
          <button
            onClick={() => setPane("chat")}
            title="Back to what you were asking"
            className="absolute right-5 top-4 z-20 flex items-center gap-2 border border-hairline bg-panel px-2.5 py-1.5 text-grey shadow-sm transition-colors hover:border-sage-dim hover:text-ink"
          >
            <PixelCluster pattern={CLUSTERS.brand} size={2.5} />
            <span className="micro">BACK TO ASK</span>
            <span className="micro text-faint">{chat.length}</span>
          </button>
        )}

        {pane === "settings" ? (
          <Settings
            settings={settings}
            onLivePreview={applyLivePreview}
            onDiarization={applyDiarization}
            onMicrophone={applyMicrophone}
            onShortcut={applyShortcut}
            meeting={meeting}
            onReplayTutorial={() => {
              setPane(null);
              // Everything, not just the unseen: somebody asking to watch it
              // again means the whole thing.
              setTutorial(everyChapter());
            }}
          />
        ) : pane === "chat" ? (
          <Chat
            turns={chat}
            onTurns={setChat}
            onForget={() => {
              void forgetChat();
              setChat([]);
            }}
            onOpenNote={(id) => void select(id)}
          />
        ) : pane === "insights" ? (
          <Insights />
        ) : job ? (
          <JobProgress job={job} onDismiss={() => setJob(null)} />
        ) : active ? (
          <TranscriptView
            transcript={active}
            onRename={rename}
            onDelete={remove}
            onEdit={editParagraphs}
            onRenameSpeaker={renameSpeaker}
            canFindSpeakers={settings?.diarization ?? false}
            naming={namingIds.has(active.id)}
            // The row in the sidebar carries the word count, and the note now
            // says something different, so both have to be re-read from the
            // store rather than patched from what we think changed.
            onReread={(id) => {
              void select(id);
              void refreshHistory(query);
            }}
          />
        ) : (
          <DropZone
            dragging={dragging}
            onBrowse={browse}
            onRecorded={(p) => runFile(p, "mic")}
            assistantError={assistantError}
            meeting={meeting}
            onStartMeeting={startMeeting}
            meetingError={meetingError}
          />
        )}

        {/* Dictation runs while other apps have focus, so this is both the
            status readout and the only place a permission failure shows up. */}
        <DictationPill />

        {/* Outside the pane switch above: a call outlives whatever was being
            read when it started, and moving to another note must not look like
            the recording stopped.

            Bottom *right*, not bottom centre. A meeting runs for an hour, and
            for that hour the card was parked in the middle of the pane — over
            the reading column, on top of the dictation pill, in the way of the
            one thing the window is for. The corner is the only edge with
            nothing already in it: the transcript's own controls (COPY, EXPORT,
            SOURCE, DELETE) live top-right, and the dictation pill is bottom
            centre. Right-aligned so a wide card and a narrow one share an
            edge instead of shuffling. */}
        <div className="pointer-events-none absolute bottom-4 right-4 z-40 flex max-w-[calc(100%-2rem)] flex-col items-end gap-2">
          <MeetingBar
            phase={meetingPhase}
            startedAt={meetingStartedAt}
            progress={meetingProgress}
            onStop={stopMeeting}
            detected={detected}
            onTakeNotes={startMeeting}
          />
        </div>

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
