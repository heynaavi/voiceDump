/** "1h 04m" / "12m 30s" / "48s" — compact but unambiguous. */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const total = Math.round(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;

  if (h > 0) return `${h}h ${String(m).padStart(2, "0")}m`;
  if (m > 0) return `${m}m ${String(s).padStart(2, "0")}s`;
  return `${s}s`;
}

/** "0:42" / "1:03:07" — for timestamps inside a transcript. */
export function formatTimestamp(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return h > 0
    ? `${h}:${mm}:${String(s).padStart(2, "0")}`
    : `${mm}:${String(s).padStart(2, "0")}`;
}

export function formatRelativeDate(ms: number): string {
  const date = new Date(ms);
  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();

  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);

  if (sameDay) {
    return date.toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
  }
  if (date.toDateString() === yesterday.toDateString()) return "Yesterday";

  const days = (now.getTime() - ms) / 86_400_000;
  if (days < 7) return date.toLocaleDateString(undefined, { weekday: "long" });
  if (date.getFullYear() === now.getFullYear()) {
    return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
  return date.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

/** Group history entries under friendly date headings. */
export function dateGroup(ms: number): string {
  const date = new Date(ms);
  const now = new Date();
  if (date.toDateString() === now.toDateString()) return "Today";

  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return "Yesterday";

  const days = (now.getTime() - ms) / 86_400_000;
  if (days < 7) return "This week";
  if (days < 30) return "This month";
  return "Earlier";
}

/** Turn "2026-03-04 client call.m4a" into "2026 03 04 client call". */
export function titleFromPath(path: string): string {
  const base = path.split("/").pop() ?? path;
  const stem = base.replace(/\.[^.]+$/, "");

  // Mic captures are named `recording-<epoch ms>` by the Rust side; a raw
  // timestamp is useless in the sidebar, so render it as a date.
  const recorded = /^recording-(\d{10,})$/.exec(stem);
  if (recorded) {
    const when = new Date(Number(recorded[1]));
    return `Recording ${when.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    })} ${when.toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    })}`;
  }

  const cleaned = stem.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim();
  return cleaned || base;
}

export function fileName(path: string): string {
  return path.split("/").pop() ?? path;
}

const MEDIA_EXTENSIONS = new Set([
  // audio
  "mp3", "m4a", "wav", "aac", "flac", "ogg", "oga", "opus", "wma", "aiff",
  "aif", "aifc", "caf", "amr", "mka", "3ga",
  // video
  "mp4", "mov", "m4v", "mkv", "webm", "avi", "wmv", "flv", "mpeg", "mpg",
  "m2ts", "mts", "ts", "3gp", "ogv",
]);

export function isSupportedMedia(path: string): boolean {
  const ext = path.split(".").pop()?.toLowerCase();
  return !!ext && MEDIA_EXTENSIONS.has(ext);
}

export const MEDIA_EXTENSION_LIST = Array.from(MEDIA_EXTENSIONS);
