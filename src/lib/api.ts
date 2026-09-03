import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Word = {
  start: number;
  end: number;
  text: string;
  /** Typed by the user rather than transcribed — tinted in the reading view. */
  edited?: boolean;
};
export type Paragraph = {
  start: number;
  end: number;
  text: string;
  words?: Word[];
  /**
   * Who was talking, on transcripts that know. Only meetings do: the two sides
   * of a call are captured separately, so attribution is a fact about which
   * track a paragraph came from rather than a guess about a voice.
   */
  speaker?: string;
  /**
   * Which track it came off: "you" or "others". Kept apart from `speaker`
   * because that label is a name somebody chose and can rename, while this is a
   * fact about which microphone heard it — and it is what the two sides are
   * coloured by. Absent on meetings recorded before it was stored.
   */
  side?: string;
  /**
   * Set once the user has typed into this paragraph. Its `words` no longer
   * describe its `text`, so follow-along drops to paragraph granularity here.
   */
  edited?: boolean;
};
export type Segment = {
  start: number;
  end: number;
  text: string;
  words?: Word[];
};

/** Where a transcript came from. Drives the origin mark in the sidebar. */
export type Origin = "file" | "mic" | "discord" | "hotkey" | "meeting";

export type TranscriptMeta = {
  id: string;
  title: string;
  source_path: string;
  duration: number;
  language: string | null;
  created_at: number;
  word_count: number;
  source: Origin;
  /** Where the media came from before archiving; "" for older transcripts. */
  origin_path: string;
};

/** Live progress for something being ingested outside the UI (e.g. Discord). */
export type IngestProgress = {
  title: string;
  stage: string;
  progress: number;
  source: Origin;
};

/** The structured overview of a note, as the model returned it. */
export type Brief = {
  summary: string;
  key_points: string[];
  action_items: { text: string; owner: string | null }[];
  decisions: string[];
};

export type Transcript = TranscriptMeta & {
  text: string;
  paragraphs: Paragraph[];
  segments: Segment[];
  /** Null on transcripts saved before the waveform existed. */
  peaks: number[] | null;
  /** Null until an overview has been generated for this note. */
  brief: Brief | null;
};

export type JobState = {
  id: string;
  path: string;
  status: "queued" | "running" | "done" | "error";
  progress: number;
  stage: string;
  error: string | null;
  result?: {
    duration: number;
    language: string | null;
    text: string;
    paragraphs: Paragraph[];
    segments: Segment[];
    peaks: number[];
    /** Which speech model ran: "small" or "medium". */
    model?: string;
    /** Milliseconds spent decoding, excluding file reading and model loading. */
    transcribe_ms?: number;
  };
};

// -- sidecar ---------------------------------------------------------------

export async function sidecarStatus(): Promise<{
  port: number | null;
  error: string | null;
}> {
  return invoke("sidecar_status");
}

// -- speech models ----------------------------------------------------------

/** What still has to be fetched before the app can transcribe anything. */
export type ModelStatus = {
  /** Nothing to do — every model this machine needs is already on disk. */
  ready: boolean;
  /** Names of the missing models, e.g. ["medium", "small"]. */
  needed: string[];
  /** Bytes still to download, for the size shown on the button. */
  bytes: number;
};

/** Emitted as `model-progress` while `modelsFetch` runs. */
export type ModelProgress = {
  label: string;
  /** 1-based, so the header can read "1 of 2". */
  index: number;
  count: number;
  received: number;
  total: number;
  /** All the bytes are here and the checksum is being computed. */
  verifying: boolean;
};

export async function modelsStatus(): Promise<ModelStatus> {
  return invoke("models_status");
}

/** Resolves only once everything has downloaded *and* verified. */
export async function modelsFetch(): Promise<void> {
  return invoke("models_fetch");
}

// -- meetings ---------------------------------------------------------------

export type MeetingCapability = {
  /** Whether this Mac can capture the far side of a call at all. */
  available: boolean;
  /** Why not, in a sentence fit to show someone. Empty when available. */
  reason: string;
  recording: boolean;
};

/** Emitted as `meeting-level`, once per 50 ms per side, while recording. */
export type MeetingLevel = {
  side: "you" | "others";
  /** 0..1, on the same scale as the dictation meter. */
  level: number;
};

/** Emitted as `meeting-progress` between stopping and saving. */
export type MeetingProgress = {
  stage: string;
  progress: number;
};

export async function meetingStatus(): Promise<MeetingCapability> {
  return invoke("meeting_status");
}

/**
 * Rejects — with a sentence worth showing — if the tap cannot be created. The
 * permission is checked by actually taking it, so a refusal surfaces here at
 * the click rather than an hour later at the save.
 */
export async function meetingStart(): Promise<void> {
  return invoke("meeting_start");
}

/**
 * Ask for the meeting to be wrapped up.
 *
 * Resolves as soon as the work is handed off, not when it is done: the outcome
 * arrives as `meeting-saved` or `meeting-failed`. It has to, because the
 * floating card can stop a meeting too and it has no promise to resolve — one
 * ending, announced the same way whoever asked for it.
 */
export async function meetingStop(): Promise<void> {
  return invoke("meeting_stop");
}

/** Recording has begun — payload is `Date.now()`-style ms at the first sample. */
export function watchMeetingStarted(on: (startedMs: number) => void) {
  return listen<number>("meeting-started", (e) => on(e.payload));
}

/** Both sides transcribed and saved; payload is the new transcript's id. */
export function watchMeetingSaved(on: (id: string) => void) {
  return listen<string>("meeting-saved", (e) => on(e.payload));
}

/** A dictation was saved. Carries its id, so the window can open it. */
export function watchDictationSaved(on: (id: string) => void) {
  return listen<string>("dictation-saved", (e) => on(e.payload));
}

/** The meeting ended without a transcript, and why. */
export function watchMeetingFailed(on: (reason: string) => void) {
  return listen<string>("meeting-failed", (e) => on(e.payload));
}

/**
 * One side of the call is missing, but the meeting is otherwise fine.
 *
 * Three moments fire this: ten seconds in when the tap has produced nothing,
 * again at the end if it never did, and at the end when a side recorded sound
 * that transcribed to nothing at all. None of them is a failure — the other
 * side is real and worth keeping — and the first is usually fixable without
 * stopping the call, which is the whole reason it is said at ten seconds
 * rather than at the save.
 */
export function watchSideMissing(on: (reason: string) => void) {
  return listen<string>("meeting-side-missing", (e) => on(e.payload));
}

/**
 * Read a note's audio again and replace what it says.
 *
 * Resolves when the new transcript is saved. Rejects with a sentence worth
 * showing, and on every failure path the existing transcript is untouched —
 * nothing is written until the decode has produced words.
 *
 * A meeting comes back as one voice: the two sides are mixed to a single track
 * when it is saved, so the words return but who said them does not. Worth
 * saying before the click, not after.
 */
export async function transcribeAgain(id: string): Promise<void> {
  return invoke("transcribe_again", { id });
}

/** How far a re-read has got. Same 0..1 contract as every other progress feed. */
export type Rereading = { id: string; stage: string; progress: number };

export function watchRereading(on: (p: Rereading) => void) {
  return listen<Rereading>("retranscribe-progress", (e) => on(e.payload));
}

/** The new transcript is saved; the overview is still being written. */
export function watchRereadDone(on: (id: string) => void) {
  return listen<string>("retranscribe-done", (e) => on(e.payload));
}

export function watchMeetingLevels(on: (level: MeetingLevel) => void) {
  return listen<MeetingLevel>("meeting-level", (e) => on(e.payload));
}

export function watchMeetingProgress(on: (p: MeetingProgress) => void) {
  return listen<MeetingProgress>("meeting-progress", (e) => on(e.payload));
}

/** macOS asks once per binary; this is the way back to that decision. */
export async function openAudioCaptureSettings(): Promise<void> {
  return invoke("open_audio_capture_settings");
}

/** An app has started using the microphone — probably a call. */
export type Detected = {
  /** Bundle identifier, e.g. `company.thebrowser.dia`. Identity, not display. */
  bundle: string;
  /** What to call it on screen, e.g. "Dia". */
  name: string;
};

export function watchMeetingDetected(on: (d: Detected) => void) {
  return listen<Detected>("meeting-detected", (e) => on(e.payload));
}

/** The floating offer was refused or timed out; drop the in-window copy. */
export function watchMeetingOfferClosed(on: () => void) {
  return listen<null>("meeting-offer-closed", () => on());
}

/** That app has let go of the microphone; the offer is stale. */
export function watchMeetingEnded(on: (bundle: string) => void) {
  return listen<string>("meeting-ended", (e) => on(e.payload));
}



export async function startJob(path: string): Promise<JobState> {
  const id = await invoke<string>("start_transcription", { path });
  return {
    id,
    path,
    status: "running",
    progress: 0,
    stage: "Starting",
    error: null,
  };
}

/**
 * Subscribe to a job's progress. Returns an unsubscribe function.
 *
 * Transcription used to live in a Python sidecar, so this was an HTTP job id
 * and an SSE stream with a polling fallback for when the stream dropped. It now
 * runs in-process, so progress is a plain Tauri event and none of that
 * plumbing — or its failure modes — exists any more.
 */
export async function watchJob(
  jobId: string,
  onUpdate: (state: JobState) => void,
): Promise<() => void> {
  const unlisten = await listen<JobState>("transcribe-progress", (event) => {
    // One listener per job, but events are app-wide: ignore other jobs.
    if (event.payload.id === jobId) onUpdate(event.payload);
  });
  return unlisten;
}

// -- history ---------------------------------------------------------------

export async function listTranscripts(query?: string): Promise<TranscriptMeta[]> {
  return invoke("list_transcripts", { query: query || null });
}

export async function getTranscript(id: string): Promise<Transcript> {
  return invoke("get_transcript", { id });
}

export async function saveTranscript(args: {
  title: string;
  sourcePath: string;
  duration: number;
  language: string | null;
  text: string;
  paragraphs: Paragraph[];
  segments: Segment[];
  peaks: number[];
  source: Origin;
  model?: string;
  transcribeMs?: number;
}): Promise<string> {
  return invoke("save_transcript", {
    title: args.title,
    sourcePath: args.sourcePath,
    duration: args.duration,
    language: args.language,
    text: args.text,
    paragraphs: args.paragraphs,
    segments: args.segments,
    peaks: args.peaks,
    source: args.source,
    model: args.model ?? null,
    transcribeMs: args.transcribeMs ?? null,
  });
}

/**
 * Compute a waveform for a file that was transcribed before peaks existed.
 * Decoded in-process; the caller should persist the result.
 */
export async function fetchPeaks(path: string): Promise<number[]> {
  return invoke<number[]>("transcribe_peaks", { path });
}

/**
 * Pull an older transcript's audio into the managed library, transcoding it to
 * a format the webview can actually play. Returns the new path.
 */
export async function archiveTranscriptMedia(id: string): Promise<string> {
  return invoke("archive_transcript_media", { id });
}

export async function setTranscriptPeaks(
  id: string,
  peaks: number[],
): Promise<void> {
  return invoke("set_transcript_peaks", { id, peaks });
}

/** Persist an inline edit. Timings, peaks and the source file are untouched. */
export async function updateTranscript(
  id: string,
  text: string,
  paragraphs: Paragraph[],
): Promise<void> {
  return invoke("update_transcript", { id, text, paragraphs });
}

/**
 * Typeset a transcript as a PDF.
 *
 * The strings are formatted here rather than in Rust so the page says exactly
 * what the window says — same duration, same word count, same timestamps — and
 * so an edit the user just made is in the export without a round trip through
 * the database.
 */
export async function exportPdf(
  dest: string,
  doc: {
    title: string;
    meta: string;
    paragraphs: { stamp: string; text: string }[];
  },
): Promise<void> {
  return invoke("export_pdf", { dest, doc });
}

/** Write a microphone recording to disk and return its path. */
export async function saveRecording(
  blob: Blob,
  extension: string,
): Promise<string> {
  const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()));
  return invoke("save_recording", { bytes, extension });
}

export async function renameTranscript(id: string, title: string): Promise<void> {
  return invoke("rename_transcript", { id, title });
}

/**
 * Name one side of a meeting, everywhere it appears.
 *
 * Answers with the whole transcript rather than an acknowledgement: every
 * paragraph and every segment changed, and the overview's action-item owners
 * along with them.
 */
/**
 * Show a note's recording in Finder.
 *
 * A command rather than the opener plugin's `revealItemInDir`, because choosing
 * *which* file to reveal means knowing which ones are still on disk, and only
 * the backend can look. See `reveal_source` for why the obvious choice is
 * usually the dead one.
 */
export function revealSource(id: string): Promise<void> {
  return invoke("reveal_source", { id });
}

export async function renameSpeaker(
  id: string,
  from: string,
  to: string,
): Promise<Transcript> {
  return invoke("rename_speaker", { id, from, to });
}

/**
 * Names spoken in a meeting, to offer when labelling a speaker.
 *
 * Empty is a normal answer — plenty of calls go by without anyone saying a
 * name — and so is a rejection, when Apple Intelligence is off. Both mean the
 * same thing on screen: type it yourself.
 */
export async function namesInMeeting(id: string): Promise<string[]> {
  return invoke("names_in_meeting", { id });
}

export async function deleteTranscript(id: string): Promise<void> {
  return invoke("delete_transcript", { id });
}

export async function writeTextFile(path: string, contents: string): Promise<void> {
  return invoke("write_text_file", { path, contents });
}

/** Write raw bytes (the share card's PNG) to a path the user picked. */
export async function writeBinaryFile(
  path: string,
  bytes: Uint8Array,
): Promise<void> {
  return invoke("write_binary_file", { path, bytes: Array.from(bytes) });
}

// -- insights ---------------------------------------------------------------

/** One row of a grouped breakdown (by app, by source, by language). */
export type Count = {
  label: string;
  notes: number;
  words: number;
  seconds: number;
};

export type DayCount = { date: string; notes: number; words: number };

/** Named `WordCount` because `Word` above is already a transcript word. */
export type WordCount = { word: string; count: number };

export type Vocabulary = {
  unique_words: number;
  total_words: number;
  variety: number;
  top_words: WordCount[];
  fillers: WordCount[];
  filler_rate: number;
  avg_sentence_words: number;
  longest_sentence_words: number;
};

export type Speaking = {
  words_per_minute: number;
  /** Audio the rate was computed from. Small samples make the rate a rumour. */
  sample_seconds: number;
  sample_notes: number;
};

export type Move = {
  key: "filler_rate" | "variety" | "avg_sentence_words" | "words_per_minute";
  before: number;
  after: number;
  /** `null` where a direction isn't self-evidently an improvement. */
  higher_is_better: boolean | null;
};
export type Progress = {
  /** "1" | "7" | "30" | "all" — the span this comparison covers. */
  key: string;
  /** False where the history is too short for this span. Shown disabled. */
  available: boolean;
  ready: boolean;
  /** Length of each half, in hours. Hours, so "all" over two days of history
   *  reads as 23-hour halves rather than rounding into the 1D tab's label. */
  window_hours: number;
  before_words: number;
  after_words: number;
  moves: Move[];
};

export type Insights = {
  total_notes: number;
  total_words: number;
  total_seconds: number;
  first_day: string | null;
  last_day: string | null;
  speaking: Speaking;
  by_day: DayCount[];
  current_streak: number;
  longest_streak: number;
  by_hour: number[];
  by_source: Count[];
  by_app: Count[];
  /** Dictations with no recorded app — pre-dating capture, or unreadable. */
  app_unknown: number;
  by_language: Count[];
  vocabulary: Vocabulary;
  /** One entry per offered span, so switching tabs needs no round trip. */
  progress: Progress[];
};

export async function analyticsSummary(): Promise<Insights> {
  return invoke("analytics_summary");
}

/** A labelled count with no duration or word total behind it. */
export type Tally = { label: string; count: number };

export type AssistantInsights = {
  ingest_by_source: Count[];
  ingest_by_day: DayCount[];
  ai_titled: number;
  ai_untitled: number;
  kb_total: number;
  kb_by_day: DayCount[];
  kb_by_channel: Tally[];
  kb_by_person: Tally[];
  kb_channels_backfilled: number;
  model_usage: ModelUse[];
  model_unmeasured: number;
  briefed: number;
  unbriefed: number;
};

/** One speech model's share of the transcription work. */
export type ModelUse = {
  label: string;
  notes: number;
  /** Seconds of audio this model transcribed. */
  seconds: number;
  /** Milliseconds it spent decoding them. */
  millis: number;
};

/**
 * The assistant-only half of Insights.
 *
 * Rejects in the lite build, where the command isn't registered at all — the
 * caller is expected to treat failure as "this build doesn't have it" and hide
 * the section, rather than surfacing an error for a feature that was never
 * meant to be there.
 */
export async function analyticsAssistant(): Promise<AssistantInsights> {
  return invoke("analytics_assistant");
}

let assistantProbe: Promise<boolean> | null = null;

/**
 * Whether this build has the AI layer at all.
 *
 * Asked of the backend rather than read from a build flag: one webview bundle
 * serves both binaries, so the only thing that knows is the command table. The
 * lite build never registers `assistant_build`, and an unknown-command
 * rejection is the answer. Memoised — it cannot change while the app runs.
 */
export function hasAssistant(): Promise<boolean> {
  assistantProbe ??= invoke<boolean>("assistant_build").then(
    () => true,
    () => false,
  );
  return assistantProbe;
}

export type BriefCapability = {
  /** Something on this machine can write an overview. */
  available: boolean;
  /** Why the on-device model can't: `apple-intelligence-off`, `os-too-old`,
   *  `device-not-eligible`, `model-not-ready`, `helper-missing`, or
   *  `available`. Worth reading even when `available` is true, because the full
   *  build can be available through Bedrock while the local model is off. */
  reason: string;
  /** The overview would be written here on this Mac, uploading nothing. */
  on_device: boolean;
  /** `reason` as a sentence with a fix in it, worded by the backend so the
   *  blocked pane and a failed attempt never disagree about why. */
  message: string;
};

/**
 * Whether an overview can be made, and where it would be made.
 *
 * Asked every time rather than memoised like {@link hasAssistant}: the answer
 * genuinely changes while the app runs. Someone who turns Apple Intelligence on
 * because this told them to should find the feature working without restarting.
 */
export async function briefCapability(): Promise<BriefCapability> {
  return invoke("brief_capability");
}

/**
 * How far through a multi-pass overview the on-device model has got.
 *
 * Carries the note's id because meetings start their own the moment they save,
 * so a reader can be looking at one note while another is being read.
 */
export type BriefProgress = { id: string; progress: number; stage: string };

export function watchBriefProgress(fn: (p: BriefProgress) => void) {
  return listen<BriefProgress>("brief-progress", (e) => fn(e.payload));
}

/** An overview finished and was stored, however it was started. */
export function watchBriefSaved(fn: (p: { id: string; brief: Brief }) => void) {
  return listen<{ id: string; brief: Brief }>("brief-saved", (e) => fn(e.payload));
}

/** An overview nobody asked for failed. The note itself is fine. */
export function watchBriefFailed(fn: (p: { id: string; problem: string }) => void) {
  return listen<{ id: string; problem: string }>("brief-failed", (e) => fn(e.payload));
}

/**
 * Generate and store the structured overview for a note.
 *
 * Takes tens of seconds, and longer on the on-device model, which reads a long
 * meeting in several passes — watch `brief-progress` rather than showing one
 * unchanging label for the whole wait. Rejects with a readable sentence for
 * every ordinary failure: nothing to summarise, Apple Intelligence switched
 * off, a Mac too old to have the model at all.
 */
export async function generateBrief(id: string): Promise<Brief> {
  return invoke("generate_brief", { id });
}

export type Theme = { label: string; note: string; share: number };
export type Themes = {
  themes: Theme[];
  sentiment: { overall: string; note: string };
  observation: string;
};

/**
 * What the recent notes are about, and how they sound.
 *
 * Resolves to null for every ordinary reason it can't answer — lite build, no
 * sidecar, no Bedrock credentials, too few notes. Only a genuine failure
 * rejects. Cached in the database against a fingerprint of the history, so
 * this is cheap unless `refresh` is set or something new was recorded.
 */
export async function analyticsThemes(refresh = false): Promise<Themes | null> {
  return invoke("analytics_themes", { refresh });
}

// -- settings ---------------------------------------------------------------

export type Settings = {
  /** Draft transcript in the overlay while you speak. On by default here. */
  live_preview: boolean;
  diarization: boolean;
  /** Microphone to record from, by name. `null` follows the system input. */
  microphone: string | null;
  /** Modifier names joined with `+` — the keys held to dictate. See lib/shortcut. */
  shortcut: string;
  /** Hold the chord for the whole recording, rather than pressing it twice. */
  hold_to_talk: boolean;
  /** Put back whatever the transcript displaced on the clipboard. */
  restore_clipboard: boolean;
};

export async function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

export async function setLivePreview(enabled: boolean): Promise<Settings> {
  return invoke("set_live_preview", { enabled });
}

export async function setDiarization(enabled: boolean): Promise<Settings> {
  return invoke("set_diarization", { enabled });
}

/**
 * Look for separate voices in one recording, and label them.
 *
 * Resolves with how many speakers were found — zero when there was only ever
 * one, which is not a failure and is the answer for most notes. Downloads the
 * models the first time, so the first call is slow in a way later ones are not.
 */
export async function findSpeakers(id: string): Promise<number> {
  return invoke("find_speakers", { id });
}

/**
 * One sentence made out of the words on the card, by the on-device model.
 *
 * Pass the words that are actually on it — anything the user struck out must
 * not be sent. Rejects when this Mac has no Apple Intelligence, and when the
 * model wrote something that was not a sentence built from these words; both
 * are ordinary outcomes, and the card is finished without one.
 */
export async function cloudSentence(words: string[]): Promise<string> {
  return invoke("cloud_sentence", { words });
}

/** How far a speaker pass has got. `id` is absent for the launch prefetch. */
export type SpeakerProgress = {
  id?: string;
  stage: "queued" | "downloading" | "verifying" | "listening";
  received?: number;
  total?: number;
  index?: number;
  count?: number;
};

/**
 * Where a speaker pass has got to, including the model download.
 *
 * Exists because the download is the slow part and used to be invisible: the
 * first recording that needed it sat on "LOOKING…" for eight minutes with
 * nothing to say it was fetching 40 MB.
 */
export function watchSpeakerProgress(
  run: (at: SpeakerProgress) => void,
): Promise<() => void> {
  return listen<SpeakerProgress>("speakers-progress", (e) => run(e.payload));
}

/**
 * The automatic speaker pass finished on a note.
 *
 * `speakers` is how many voices were labelled, and `0` — meaning one voice, or
 * no models yet, or nothing usable — is the ordinary answer rather than an
 * error. Only fires for recordings brought in as files, and only while the
 * Find speakers setting is on.
 */
export function watchSpeakersFound(
  run: (found: { id: string; speakers: number }) => void,
): Promise<() => void> {
  return listen<{ id: string; speakers: number }>("speakers-found", (e) =>
    run(e.payload),
  );
}

/** One attached input device. */
export type Mic = {
  name: string;
  /** Whether macOS would pick this one right now. */
  is_default: boolean;
};

/** Microphones attached at this moment — ask again each time the picker opens. */
export async function listMicrophones(): Promise<Mic[]> {
  return invoke("list_microphones");
}

/** Pass `null` to follow whatever macOS is set to. */
export async function setMicrophone(name: string | null): Promise<Settings> {
  return invoke("set_microphone", { name });
}

/**
 * Choose the keys held to dictate.
 *
 * Rejects with a readable sentence if the chord isn't one the keyboard tap can
 * watch for — a lone modifier, or anything containing a non-modifier key.
 */
export async function setShortcut(chord: string): Promise<Settings> {
  return invoke("set_shortcut", { chord });
}

/**
 * Hold the chord down, or press it twice.
 *
 * `true` is push-to-talk, which is what the app has always done. `false` makes
 * the chord a switch: press and release to start, again to stop.
 */
export async function setHoldToTalk(enabled: boolean): Promise<Settings> {
  return invoke("set_hold_to_talk", { enabled });
}

/**
 * Whether a dictation gives you your clipboard back.
 *
 * `false` leaves the transcript on it instead of restoring what was there.
 */
export async function setRestoreClipboard(enabled: boolean): Promise<Settings> {
  return invoke("set_restore_clipboard", { enabled });
}

// -- asking the library ------------------------------------------------------

export type Topic = {
  id: number;
  kind: "person" | "project" | "topic" | "org";
  name: string;
  /** How many notes mention it. What separates a subject from a stray phrase. */
  mentions: number;
  last_seen: number;
};

export type AnswerSource = {
  id: string;
  title: string;
  created_at: number;
  /** Which route found this note: matched on subject, or on words. */
  via: "topic" | "search";
};

/** One thing the notes said, and which note said it. */
export type AnswerPoint = {
  says: string;
  /** 1-based index into `sources`. Zero means no note stood behind it. */
  note: number;
};

export type Answer = {
  /** The whole answer as flat prose — and the only field the unstructured
   *  paths fill: social, meta, no-model, retrieved-only, nothing-matched. */
  text: string;
  /** Optional because turns stored before the typed contract have neither. */
  headline?: string;
  points?: AnswerPoint[];
  sources: AnswerSource[];
  /** No model wrote this: the notes were found and the finding is the answer.
   *  True on a Mac without Apple Intelligence, where retrieval still works. */
  retrieved_only: boolean;
};

/** What one note is about. */
export async function noteTopics(id: string): Promise<Topic[]> {
  return invoke("note_topics", { id });
}

/** What the library keeps coming back to, most-mentioned first. */
export async function libraryTopics(
  kind?: Topic["kind"],
  limit?: number,
): Promise<Topic[]> {
  return invoke("library_topics", { kind: kind ?? null, limit: limit ?? null });
}

/**
 * Ask the library a question.
 *
 * Slow — retrieval is instant, the on-device model is not — so callers show a
 * thinking state rather than awaiting this in a click handler.
 */
export async function askLibrary(question: string): Promise<Answer> {
  return invoke("ask_library", { question });
}

/**
 * Turn a recording into words without keeping it.
 *
 * The chat's microphone. Every other transcription in this app makes a note;
 * this one fills a text field, and the scratch file is deleted either way.
 */
export async function transcribeOnce(path: string): Promise<string> {
  return invoke("transcribe_once", { path });
}

/**
 * A stage of answering, as it happens.
 *
 * Not a chain of thought — this model exposes none, and inventing one would be
 * a lie about what the machine is doing. These are the real steps: searching,
 * which notes came back, reading them, writing, and the plain-instructions
 * retry on the runs where the first wording produced a tool call.
 */
export type AskStage = {
  stage:
    | "reading-you"
    | "searching"
    | "reading"
    | "writing"
    | "retrying"
    | "nothing"
    | "no-model";
  /** The notes read, why there is no model, or the words being searched for. */
  detail: AnswerSource[] | { reason: string } | { terms: string[] } | null;
};

export function watchAskProgress(on: (step: AskStage) => void) {
  const off = listen<AskStage>("ask-progress", (e) => on(e.payload));
  return () => {
    off.then((f) => f());
  };
}

/** One question and whatever came back, as kept on disk. */
export type StoredTurn = {
  id: number;
  question: string;
  /** The whole answer, or null when the ask failed outright. */
  answer: Answer | null;
  error: string;
  asked_at: number;
};

/** Everything asked so far, oldest first. */
export async function chatHistory(): Promise<StoredTurn[]> {
  return invoke("chat_history");
}

/** Throw the conversation away. */
export async function forgetChat(): Promise<void> {
  return invoke("forget_chat");
}

// -- version and updates ----------------------------------------------------

/** The running version, from the bundle. */
export async function appVersion(): Promise<string> {
  return invoke("app_version");
}

export type Update = { current: string; latest: string; newer: boolean };

/**
 * Ask GitHub whether a newer release exists.
 *
 * The only outbound call the app makes after the models are fetched, and it
 * carries nothing about the user — a GET for a public release listing. Called
 * from a click, and once a day by the sidebar; see `LOOKED_AT` there for why
 * the once-a-day is remembered on disk rather than run on every launch.
 */
export async function checkUpdate(): Promise<Update> {
  return invoke("check_update");
}

/**
 * Open a release page in the browser.
 *
 * Takes a version, not a URL: the backend re-validates it and builds the
 * address from a fixed repository, so nothing the network said can decide
 * where this goes.
 */
/**
 * Open the update page in the browser.
 *
 * Takes no version: the running one is read from the bundle on the other side,
 * which knows it better than this window does and cannot be talked out of it.
 */
export async function openRelease(): Promise<void> {
  return invoke("open_release");
}

/**
 * The app working through a backlog of AI work on an existing library.
 *
 * `left` counts down and reaches 0 when that pass is done; `doing` is empty on
 * the final event, which means the whole chain has finished. There is no total
 * and no percentage on purpose — see `catching_up` in lib.rs.
 */
export type CatchingUp = { doing: string; left: number };

export function watchCatchingUp(on: (c: CatchingUp) => void) {
  const off = listen<CatchingUp>("catching-up", (e) => on(e.payload));
  return () => {
    off.then((f) => f());
  };
}
