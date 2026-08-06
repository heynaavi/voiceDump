import { useCallback, useEffect, useRef, useState } from "react";

import {
  askLibrary,
  getSettings,
  libraryTopics,
  saveRecording,
  transcribeOnce,
  watchAskProgress,
  type AskStage,
  type Answer,
  type AnswerSource,
  type Topic,
} from "../lib/api";
import { formatWhen } from "../lib/format";
import { CLUSTERS, PixelCluster } from "./PixelCluster";

/**
 * Ask — the library, answered out of the library.
 *
 * Three things this view refuses to do:
 *
 * **It won't answer from anything but your notes.** The model is given six of
 * them and told to use nothing else. When they don't contain the answer it says
 * so, and that reply is a success rather than a failure — the entire value of
 * asking your own recordings is that the answer is yours, and a plausible
 * invention is indistinguishable from a real recollection until it matters.
 *
 * **It won't hide where an answer came from.** Every reply carries the notes it
 * was built from, each one clickable. An answer you can't check is a rumour.
 *
 * **It won't pretend to think.** The wait shows the stages that actually ran —
 * searching, which six notes came back and by which route, reading, writing —
 * and nothing else. This model reports no chain of thought, and a made-up one
 * would be a lie about what the machine is doing while you watch it.
 *
 * The conversation itself belongs to the app, not to this component: opening a
 * cited note unmounts this pane, and a chat that forgets itself the moment you
 * check a citation is a chat nobody can have a second question in.
 */

export type Turn = {
  id: number;
  question: string;
  /** Absent while the model is still reading. */
  answer: Answer | null;
  error: string | null;
};

type Props = {
  /** Open a note from a citation. */
  onOpenNote: (id: string) => void;
  /** Held above this component so it survives opening a note. */
  turns: Turn[];
  onTurns: (next: (current: Turn[]) => Turn[]) => void;
  /** Clear the kept conversation. */
  onForget: () => void;
};

/** WebKit reliably produces MP4/AAC; webm is here for a Chromium webview. */
const MIME_CANDIDATES = [
  { mime: "audio/mp4", ext: "m4a" },
  { mime: "audio/webm;codecs=opus", ext: "webm" },
  { mime: "audio/webm", ext: "webm" },
];

function pickMime() {
  if (typeof MediaRecorder === "undefined") return null;
  return MIME_CANDIDATES.find((c) => MediaRecorder.isTypeSupported(c.mime)) ?? null;
}

/**
 * Speak a question instead of typing it.
 *
 * This is a voice app, so the question arrives the same way the notes did. It
 * deliberately does *not* go through the normal ingest: what you said was a
 * question, the answer is the thing worth keeping, and a library filling up
 * with one-line questions is not a library. Rust deletes the scratch file.
 */
function useVoiceQuestion(onHeard: (text: string) => void) {
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const recorder = useRef<MediaRecorder | null>(null);
  const chunks = useRef<Blob[]>([]);

  const stop = useCallback(() => {
    recorder.current?.stop();
    setRecording(false);
  }, []);

  const start = useCallback(async () => {
    setProblem(null);
    const picked = pickMime();
    if (!picked) {
      setProblem("This webview can't record audio.");
      return;
    }
    try {
      const preferred = (await getSettings().catch(() => null))?.microphone ?? null;
      const devices = preferred
        ? await navigator.mediaDevices.enumerateDevices().catch(() => [])
        : [];
      const deviceId = devices.find(
        (d) => d.kind === "audioinput" && d.label === preferred,
      )?.deviceId;

      const stream = await navigator.mediaDevices.getUserMedia({
        audio: deviceId
          ? { deviceId: { exact: deviceId }, echoCancellation: true, noiseSuppression: true }
          : { echoCancellation: true, noiseSuppression: true },
      });

      chunks.current = [];
      const rec = new MediaRecorder(stream, { mimeType: picked.mime });
      rec.ondataavailable = (e) => {
        if (e.data.size) chunks.current.push(e.data);
      };
      rec.onstop = async () => {
        // Released before transcribing, not after: the macOS recording
        // indicator staying lit through a ten-second transcription reads as the
        // app still listening.
        stream.getTracks().forEach((t) => t.stop());
        setTranscribing(true);
        try {
          const blob = new Blob(chunks.current, { type: picked.mime });
          const path = await saveRecording(blob, picked.ext);
          const heard = await transcribeOnce(path);
          if (heard.trim()) onHeard(heard.trim());
          else setProblem("Didn't catch that.");
        } catch (e) {
          setProblem(String(e));
        } finally {
          setTranscribing(false);
        }
      };
      rec.start();
      recorder.current = rec;
      setRecording(true);
    } catch {
      setProblem("Couldn't open the microphone.");
    }
  }, [onHeard]);

  useEffect(() => {
    return () => {
      recorder.current?.stream?.getTracks().forEach((t) => t.stop());
    };
  }, []);

  return { recording, transcribing, problem, start, stop };
}

/**
 * Render `[1]` and `[2]` as the notes they point at.
 *
 * The citation is the whole reason to trust the answer, so it is a control
 * rather than punctuation. A number with no matching source is left as plain
 * text — the model occasionally cites a seventh note when it was given six, and
 * a button that opens nothing is worse than a bracket.
 */
function Cited({
  text,
  sources,
  onOpenNote,
}: {
  text: string;
  sources: AnswerSource[];
  onOpenNote: (id: string) => void;
}) {
  const parts = text.split(/(\[\d+\])/g);
  return (
    <p className="selectable whitespace-pre-wrap text-[13px] leading-relaxed text-ink">
      {parts.map((part, i) => {
        const match = /^\[(\d+)\]$/.exec(part);
        const source = match ? sources[Number(match[1]) - 1] : undefined;
        if (!source) return <span key={i}>{part}</span>;
        return (
          <button
            key={i}
            onClick={() => onOpenNote(source.id)}
            title={source.title}
            className="mx-0.5 border border-sage-dim/50 px-1 align-baseline text-[10px] text-sage-dim transition-colors hover:bg-sage-dim/20 hover:text-ink"
          >
            {match![1]}
          </button>
        );
      })}
    </p>
  );
}

/** Lowercase, trimmed, no trailing full stop — for comparing two sentences. */
function norm(s: string) {
  return s.trim().toLowerCase().replace(/\.$/, "");
}

/**
 * An answer, however much shape it turned out to have.
 *
 * One predicate rather than a list of cases: no points means render the prose.
 * That single rule covers a greeting, the meta reply, a Mac with no model, a
 * question nothing matched, a reply that came back as prose, and every turn
 * stored before the typed contract existed.
 *
 * With points, the citation chip comes from `point.note` rather than being
 * scraped out of the sentence — so it cannot drift onto the wrong claim, and
 * cannot arrive as "(2)" instead of "[2]".
 */
function Structured({
  answer,
  onOpenNote,
}: {
  answer: Answer;
  onOpenNote: (id: string) => void;
}) {
  const points = answer.points ?? [];
  const headline = answer.headline ?? "";

  if (points.length === 0) {
    return (
      <Cited text={answer.text} sources={answer.sources} onOpenNote={onOpenNote} />
    );
  }

  // A single point that just restates the headline would print the same
  // sentence twice. The *headline* is what goes — dropping the point instead
  // destroys its citation, which on real answers was 16 cases out of 18.
  const lead =
    headline &&
    !(points.length === 1 && norm(points[0].says) === norm(headline));

  return (
    <div className="flex flex-col gap-2">
      {lead && (
        <p className="selectable text-[13px] font-medium leading-relaxed text-ink">
          {headline}
        </p>
      )}
      <ul className="flex flex-col gap-1.5">
        {points.map((p, i) => {
          const source = p.note > 0 ? answer.sources[p.note - 1] : undefined;
          return (
            <li key={i} className="flex items-baseline gap-2">
              <span className="mt-[1px] shrink-0 text-faint">
                <PixelCluster pattern={CLUSTERS.bullet} size={2} />
              </span>
              <span className="selectable text-[13px] leading-relaxed text-ink">
                {p.says}
                {source && (
                  <button
                    onClick={() => onOpenNote(source.id)}
                    title={source.title}
                    className="mx-1 border border-sage-dim/50 px-1 align-baseline text-[10px] text-sage-dim transition-colors hover:bg-sage-dim/20 hover:text-ink"
                  >
                    {p.note}
                  </button>
                )}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** What each real stage is called, in the second person, while it happens. */
const STAGE_WORDS: Record<AskStage["stage"], string> = {
  // Named because it is real work and it is the step most likely to be wrong.
  // Deciding what you meant now takes a model call of its own, and when the
  // answer surprises you — a question answered without a search, a follow-up
  // that went looking — this is the step that decided it.
  "reading-you": "Working out what you meant",
  searching: "Searching your notes",
  reading: "Reading",
  writing: "Writing the answer",
  // Named rather than hidden. The first wording sometimes makes this model
  // answer with a tool call instead of a sentence; when that happens the retry
  // is a real second or two of waiting, and an unexplained pause is worse than
  // an explained one.
  retrying: "That came back oddly — asking again, more plainly",
  nothing: "Nothing matched",
  "no-model": "Apple Intelligence is off — showing what matched",
};

/**
 * The work, as it happens.
 *
 * Stages are appended rather than replaced, so the finished trace reads as an
 * account of the run: what was searched, which notes came back, whether the
 * retry fired. On a fast answer this is a flicker; on a slow one it is the
 * difference between waiting and wondering whether it has hung.
 */
function Trace({
  steps,
  onOpenNote,
}: {
  steps: AskStage[];
  onOpenNote: (id: string) => void;
}) {
  const found = steps.find((s) => s.stage === "reading");
  const notes = Array.isArray(found?.detail) ? found.detail : [];
  const last = steps[steps.length - 1];

  return (
    <div className="flex flex-col gap-2 border-l border-hairline pl-3">
      {steps.map((step, i) => {
        const current = i === steps.length - 1;
        return (
          <p
            key={i}
            className={`micro flex items-center gap-2 ${
              current ? "text-grey" : "text-faint"
            }`}
          >
            <span className={current ? "text-sage-dim" : "text-faint"}>
              <PixelCluster
                pattern={current ? CLUSTERS.brand : CLUSTERS.done}
                size={2.5}
                pulse={current}
              />
            </span>
            {STAGE_WORDS[step.stage].toUpperCase()}
            {step.stage === "reading" && ` ${notes.length} NOTE${notes.length === 1 ? "" : "S"}`}
          </p>
        );
      })}

      {/* Named while they are being read, not after. Which six notes were
          picked is the part of this most likely to be wrong, and the only
          moment it is inspectable is now. */}
      {notes.length > 0 && last?.stage !== "nothing" && (
        <div className="flex flex-wrap gap-1.5 pt-0.5">
          {notes.map((n, i) => (
            <button
              key={n.id}
              onClick={() => onOpenNote(n.id)}
              title={n.via === "topic" ? "matched on subject" : "matched on words"}
              className="flex items-center gap-1.5 border border-hairline px-2 py-1 text-[11px] text-faint transition-colors hover:border-sage-dim hover:text-ink"
            >
              <span>{i + 1}</span>
              <span className="max-w-[160px] truncate">{n.title}</span>
              <span className={n.via === "topic" ? "text-sage-dim" : "text-faint"}>
                {n.via === "topic" ? "SUBJECT" : "WORDS"}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function Chat({ onOpenNote, turns, onTurns, onForget }: Props) {
  const [draft, setDraft] = useState("");
  const [clearing, setClearing] = useState(false);
  const [thinking, setThinking] = useState(false);
  const [steps, setSteps] = useState<AskStage[]>([]);
  const [topics, setTopics] = useState<Topic[]>([]);
  const nextId = useRef(1);
  const foot = useRef<HTMLDivElement | null>(null);
  const field = useRef<HTMLTextAreaElement | null>(null);

  // A turn on screen is given a temporary id until the backend writes it down
  // and hands back the real one, so the counter only has to clear whatever is
  // already here — including a history loaded from disk with ids of its own.
  useEffect(() => {
    nextId.current = Math.max(nextId.current, ...turns.map((t) => t.id + 1));
    // Once, on mount. Later turns advance it themselves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // What the library is about, as an opening move. A blank chat box over
  // somebody's own notes is a harder question than it looks — these are the
  // subjects that actually have notes behind them, so every one is answerable.
  useEffect(() => {
    libraryTopics(undefined, 12)
      .then((all) => setTopics(all.filter((t) => t.mentions > 1)))
      .catch(() => setTopics([]));
  }, []);

  // Subscribed for the life of the pane rather than per question: the events
  // start arriving inside the same call that sets `thinking`, and a listener
  // attached after the invoke has already missed "searching".
  useEffect(() => {
    return watchAskProgress((step) => setSteps((all) => [...all, step]));
  }, []);

  useEffect(() => {
    foot.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [turns, thinking, steps]);

  const ask = useCallback(
    async (question: string) => {
      const trimmed = question.trim();
      if (!trimmed) return;

      const id = nextId.current++;
      onTurns((t) => [...t, { id, question: trimmed, answer: null, error: null }]);
      setDraft("");
      setSteps([]);
      setThinking(true);
      try {
        const answer = await askLibrary(trimmed);
        onTurns((t) => t.map((x) => (x.id === id ? { ...x, answer } : x)));
      } catch (e) {
        onTurns((t) => t.map((x) => (x.id === id ? { ...x, error: String(e) } : x)));
      } finally {
        setThinking(false);
        setSteps([]);
      }
    },
    [onTurns],
  );

  const voice = useVoiceQuestion(
    useCallback(
      (heard: string) => {
        // Asked straight off, not dropped in the box to be confirmed. Speaking a
        // question and then reaching for the keyboard to send it is the worst of
        // both inputs.
        void ask(heard);
      },
      [ask],
    ),
  );

  const busy = thinking || voice.transcribing;

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-start justify-between gap-4 border-b border-hairline px-6 py-4">
        <div>
          <h1 className="micro text-grey">ASK</h1>
          <p className="mt-1 text-[13px] text-faint">
            Answered from your own recordings, and nothing else.
          </p>
        </div>

        {/* A log of everything somebody has asked about their own recordings,
            kept forever with no way to clear it, is a liability dressed as a
            feature. Two presses, because one press that erases a conversation
            is the wrong number. */}
        {turns.length > 0 && (
          <button
            onClick={() => {
              if (clearing) {
                onForget();
                setClearing(false);
              } else {
                setClearing(true);
              }
            }}
            onBlur={() => setClearing(false)}
            className={`micro shrink-0 border px-2.5 py-1.5 transition-colors ${
              clearing
                ? "border-clay bg-clay/10 text-clay"
                : "border-hairline text-faint hover:border-clay hover:text-clay"
            }`}
          >
            {clearing ? "ERASE EVERYTHING?" : "CLEAR"}
          </button>
        )}
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
        {turns.length === 0 && (
          <div className="mx-auto max-w-[560px] pt-6">
            <p className="text-[13px] text-faint">
              Ask what was decided, what someone said, when something came up.
              Every answer cites the notes it came from.
            </p>
            {topics.length > 0 && (
              <>
                <p className="micro mt-6 text-grey">WHAT YOU KEEP COMING BACK TO</p>
                <div className="mt-2 flex flex-wrap gap-1.5">
                  {topics.map((t) => (
                    <button
                      key={t.id}
                      onClick={() => void ask(`What did I say about ${t.name}?`)}
                      className="border border-hairline px-2 py-1 text-[11px] text-grey transition-colors hover:border-sage-dim hover:text-ink"
                    >
                      {t.name}
                      <span className="ml-1.5 text-faint">{t.mentions}</span>
                    </button>
                  ))}
                </div>
              </>
            )}
          </div>
        )}

        <div className="mx-auto flex max-w-[560px] flex-col gap-5">
          {turns.map((turn) => (
            <div key={turn.id} className="flex flex-col gap-2">
              <p className="selectable text-[13px] text-grey">{turn.question}</p>

              {turn.answer && (
                <div className="border-l border-sage-dim/40 pl-3">
                  {/* Said before the text, not after: without this a list of
                      notes reads as something the model concluded, and on a Mac
                      with no model nothing concluded anything. */}
                  {turn.answer.retrieved_only && (
                    <p className="micro mb-1.5 text-amber">
                      NOT ANSWERED — THESE NOTES MATCHED
                    </p>
                  )}
                  <Structured answer={turn.answer} onOpenNote={onOpenNote} />
                  {turn.answer.sources.length > 0 && (
                    <div className="mt-2.5 flex flex-wrap gap-1.5">
                      {turn.answer.sources.map((s, i) => (
                        <button
                          key={s.id}
                          onClick={() => onOpenNote(s.id)}
                          className="flex items-center gap-1.5 border border-hairline px-2 py-1 text-left text-[11px] text-grey transition-colors hover:border-sage-dim hover:text-ink"
                        >
                          <span className="text-faint">{i + 1}</span>
                          <span className="max-w-[180px] truncate">{s.title}</span>
                          <span className="text-faint">{formatWhen(s.created_at)}</span>
                        </button>
                      ))}
                    </div>
                  )}

                  {/* Offered only where trying again is a real prospect. The
                      commonest reason an answer doesn't land is Apple's safety
                      model being momentarily absent, which fixes itself — the
                      same condition left 26 notes unsummarised one launch and
                      none the next. A retry on a question that simply matched
                      nothing would just be a button that does nothing twice. */}
                  {turn.answer.retrieved_only && turn.answer.sources.length > 0 && (
                    <button
                      onClick={() => void ask(turn.question)}
                      disabled={busy}
                      className="micro mt-2.5 border border-hairline px-2.5 py-1 text-grey transition-colors hover:border-sage-dim hover:text-ink disabled:opacity-40"
                    >
                      ASK AGAIN
                    </button>
                  )}
                </div>
              )}

              {turn.error && (
                <p className="border-l border-clay/50 pl-3 text-[13px] text-clay">
                  {turn.error}
                </p>
              )}
            </div>
          ))}

          {voice.transcribing && (
            <p className="micro animate-pulse text-faint">TRANSCRIBING</p>
          )}
          {thinking &&
            (steps.length > 0 ? (
              <Trace steps={steps} onOpenNote={onOpenNote} />
            ) : (
              // The gap before the first event lands. Named for what is
              // actually happening rather than left blank.
              <p className="micro animate-pulse text-faint">SEARCHING YOUR NOTES</p>
            ))}
          <div ref={foot} />
        </div>
      </div>

      <div className="border-t border-hairline px-6 py-3">
        <div className="mx-auto flex max-w-[560px] items-end gap-2">
          <textarea
            ref={field}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              // Enter sends; shift-enter is a newline. A question is one line
              // far more often than it is a paragraph.
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void ask(draft);
              }
            }}
            rows={1}
            placeholder={voice.recording ? "Listening…" : "Ask your notes"}
            disabled={voice.recording}
            spellCheck={false}
            className="selectable max-h-32 min-h-[38px] flex-1 resize-none border border-hairline bg-panel px-3 py-2 text-[13px] text-ink outline-none placeholder:text-faint focus:border-sage-dim disabled:text-faint"
          />

          <button
            onClick={() => (voice.recording ? voice.stop() : void voice.start())}
            disabled={busy}
            aria-pressed={voice.recording}
            title={voice.recording ? "Stop and ask" : "Ask out loud"}
            className={`flex h-[38px] w-[38px] items-center justify-center border transition-colors disabled:opacity-40 ${
              voice.recording
                ? "border-clay bg-clay/15 text-clay"
                : "border-hairline text-grey hover:border-sage-dim hover:text-ink"
            }`}
          >
            <PixelCluster
              pattern={voice.recording ? CLUSTERS.done : CLUSTERS.brand}
              size={3}
            />
          </button>

          <button
            onClick={() => void ask(draft)}
            disabled={busy || !draft.trim()}
            className="h-[38px] border border-ink bg-ink px-3 text-surface transition-colors hover:bg-transparent hover:text-ink disabled:opacity-40 disabled:hover:bg-ink disabled:hover:text-surface"
          >
            <span className="micro">ASK</span>
          </button>
        </div>

        {voice.problem && (
          <p className="mx-auto mt-2 max-w-[560px] text-[11px] text-clay">
            {voice.problem}
          </p>
        )}
      </div>
    </div>
  );
}
