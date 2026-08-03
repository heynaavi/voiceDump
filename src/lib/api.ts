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
export type Origin = "file" | "mic" | "hotkey";

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

/** Live progress for something being transcribed outside the UI. */
export type IngestProgress = {
  title: string;
  stage: string;
  progress: number;
  source: Origin;
};

export type Transcript = TranscriptMeta & {
  text: string;
  paragraphs: Paragraph[];
  segments: Segment[];
  /** Null on transcripts saved before the waveform existed. */
  peaks: number[] | null;
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

// -- engine ----------------------------------------------------------------

/** Is the app able to transcribe at all? Asked once, on boot. */
export async function engineHealth(): Promise<{ error: string | null }> {
  return invoke("engine_health");
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
 * Transcription runs in-process, so this is a local job id
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

export async function deleteTranscript(id: string): Promise<void> {
  return invoke("delete_transcript", { id });
}

export async function writeTextFile(path: string, contents: string): Promise<void> {
  return invoke("write_text_file", { path, contents });
}

/** Save bytes (the share card PNG) to a path the user chose. */
export async function writeBinaryFile(
  path: string,
  bytes: Uint8Array,
): Promise<void> {
  return invoke("write_binary_file", { path, bytes: Array.from(bytes) });
}

/**
 * Show a transcript's audio in Finder.
 *
 * The backend decides which of the two paths to open — the original if the user
 * still has it, the library copy otherwise — because only it can check what is
 * on disk. It rejects when neither exists, and that rejection is worth showing:
 * this silently did nothing for a long time.
 */
export async function revealSource(
  origin: string,
  archived: string,
): Promise<void> {
  return invoke("reveal_source", { origin, archived });
}

// -- settings ---------------------------------------------------------------

export type Settings = {
  /** Draft transcript in the overlay while you speak. Off by default. */
  live_preview: boolean;
  /** Microphone to record from, by name. `null` follows the system input. */
  microphone: string | null;
  /** Modifier names joined with `+` — the keys held to dictate. See lib/shortcut. */
  shortcut: string;
};

export async function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

export async function setLivePreview(enabled: boolean): Promise<Settings> {
  return invoke("set_live_preview", { enabled });
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

// -- version ----------------------------------------------------------------

/** The running version, from the bundle. */
export async function appVersion(): Promise<string> {
  return invoke("app_version");
}

export type Update = { current: string; latest: string; newer: boolean };

/**
 * Ask GitHub whether a newer release exists.
 *
 * The only network call the app makes, and only ever from a click. Nothing
 * about the user is sent — it is a GET for a public release listing.
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
export async function openRelease(version: string): Promise<void> {
  return invoke("open_release", { version });
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
  /** One per offered span, shortest first. Switching is local, not a refetch. */
  progress: Progress[];
};

export async function analyticsSummary(): Promise<Insights> {
  return invoke("analytics_summary");
}
