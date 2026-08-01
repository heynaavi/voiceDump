import { useCallback, useEffect, useRef, useState } from "react";

import { getSettings, saveRecording } from "../lib/api";
import { formatTimestamp } from "../lib/format";

type Props = {
  /** Called with the written file's path once recording stops. */
  onCaptured: (path: string) => void;
};

/**
 * WebKit only reliably produces MP4/AAC; the webm entries are here so the same
 * component keeps working if this ever runs in a Chromium webview.
 */
const MIME_CANDIDATES = [
  { mime: "audio/mp4", ext: "m4a" },
  { mime: "audio/webm;codecs=opus", ext: "webm" },
  { mime: "audio/webm", ext: "webm" },
];

function pickMime() {
  if (typeof MediaRecorder === "undefined") return null;
  return (
    MIME_CANDIDATES.find((c) => MediaRecorder.isTypeSupported(c.mime)) ?? null
  );
}

const CLEANUP = {
  echoCancellation: true,
  noiseSuppression: true,
  autoGainControl: true,
};

/**
 * Open the microphone the user chose, in the currency this side speaks.
 *
 * The two capture paths name devices differently: CoreAudio hands Rust a device
 * *name*, and `getUserMedia` wants a `deviceId` that is salted per origin — so
 * the setting cannot store one. The bridge is `label`, which is the same string
 * CoreAudio uses.
 *
 * Labels are blank until the page has been granted the microphone, and the
 * picker is filled by Rust, so the first recording after choosing a device can
 * land here with nothing to match against. Opening any stream is what reveals
 * the labels — hence the second look. Every failure falls back to the default
 * device rather than refusing to record, which is what the globe key does too.
 */
async function openMic(preferred: string | null): Promise<MediaStream> {
  const open = (deviceId?: string) =>
    navigator.mediaDevices.getUserMedia({
      audio: deviceId ? { ...CLEANUP, deviceId: { exact: deviceId } } : CLEANUP,
    });

  if (!preferred) return open();

  const tried = new Set<string>();
  const attempt = async () => {
    const devices = await navigator.mediaDevices.enumerateDevices();
    const id = devices.find(
      (d) => d.kind === "audioinput" && d.label === preferred,
    )?.deviceId;
    if (!id || tried.has(id)) return null;
    tried.add(id);
    // An id can go stale between listing and opening — the interface was
    // unplugged, or is asleep. Not an error, just not that microphone.
    return open(id).catch(() => null);
  };

  const direct = await attempt();
  if (direct) return direct;

  const fallback = await open();
  if (fallback.getAudioTracks()[0]?.label === preferred) return fallback;
  const second = await attempt();
  if (!second) return fallback;
  // Two streams are briefly live; the loser is released immediately so macOS
  // drops the recording indicator for it.
  fallback.getTracks().forEach((t) => t.stop());
  return second;
}

/** Live level meter, so it's obvious the mic is actually picking something up. */
function useLevel(stream: MediaStream | null) {
  const [level, setLevel] = useState(0);

  useEffect(() => {
    if (!stream) {
      setLevel(0);
      return;
    }
    const ctx = new AudioContext();
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 512;
    ctx.createMediaStreamSource(stream).connect(analyser);
    const buf = new Uint8Array(analyser.frequencyBinCount);

    let raf = 0;
    const tick = () => {
      analyser.getByteTimeDomainData(buf);
      let sum = 0;
      for (const v of buf) sum += (v - 128) ** 2;
      // Scaled well above raw RMS: speech sits low on this scale and an
      // honest-but-invisible meter reads as a broken mic.
      setLevel(Math.min(1, Math.sqrt(sum / buf.length) / 32));
      raf = requestAnimationFrame(tick);
    };
    tick();

    return () => {
      cancelAnimationFrame(raf);
      ctx.close().catch(() => {});
    };
  }, [stream]);

  return level;
}

export function Recorder({ onCaptured }: Props) {
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const chunks = useRef<Blob[]>([]);
  const level = useLevel(stream);

  const recording = stream !== null;

  useEffect(() => {
    if (!recording) return;
    setElapsed(0);
    const started = Date.now();
    const t = setInterval(() => setElapsed((Date.now() - started) / 1000), 200);
    return () => clearInterval(t);
  }, [recording]);

  // Releasing the mic matters: macOS keeps the orange indicator lit and holds
  // the device open for as long as any track is live.
  useEffect(
    () => () => {
      recorderRef.current?.stream.getTracks().forEach((t) => t.stop());
    },
    [],
  );

  const start = useCallback(async () => {
    setError(null);
    const picked = pickMime();
    if (!picked) {
      setError("This webview can't record audio.");
      return;
    }

    let media: MediaStream;
    try {
      // The setting lives in Rust, so it is read fresh rather than held in
      // state: it can be changed from the sidebar between two recordings.
      const chosen = await getSettings()
        .then((s) => s.microphone)
        .catch(() => null);
      media = await openMic(chosen);
    } catch {
      setError(
        "Microphone access was denied. Grant it in System Settings › Privacy & Security › Microphone.",
      );
      return;
    }

    chunks.current = [];
    const rec = new MediaRecorder(media, { mimeType: picked.mime });
    rec.ondataavailable = (e) => {
      if (e.data.size) chunks.current.push(e.data);
    };
    rec.onstop = async () => {
      media.getTracks().forEach((t) => t.stop());
      setStream(null);
      const blob = new Blob(chunks.current, { type: picked.mime });
      if (!blob.size) {
        setError("Nothing was captured.");
        return;
      }
      try {
        onCaptured(await saveRecording(blob, picked.ext));
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    };

    recorderRef.current = rec;
    rec.start();
    setStream(media);
  }, [onCaptured]);

  const stop = useCallback(() => {
    recorderRef.current?.stop();
  }, []);

  if (!recording) {
    return (
      <div className="flex flex-col items-center gap-2">
        <button
          onClick={start}
          className="micro flex items-center gap-2.5 border border-hairline px-4 py-2.5 text-grey transition-colors hover:border-ink hover:bg-ink hover:text-surface"
        >
          <span className="block h-2.5 w-2.5 bg-amber" />
          RECORD FROM MIC
        </button>
        {error && (
          <p className="micro max-w-[320px] text-center leading-relaxed text-amber">
            {error}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className="flex items-center gap-3 border border-ink bg-panel px-3 py-2.5">
      <button
        onClick={stop}
        aria-label="Stop recording"
        className="flex h-9 w-9 shrink-0 items-center justify-center border border-amber bg-amber text-surface transition-colors hover:bg-transparent hover:text-amber"
      >
        <span className="block h-3 w-3 bg-current" />
      </button>

      <span className="mono-data w-16 text-[13px] tabular-nums text-ink">
        {formatTimestamp(elapsed)}
      </span>

      {/* Level meter in the pixel language — discrete cells, no smooth bar. */}
      <div className="flex items-center gap-[2px]">
        {Array.from({ length: 18 }, (_, i) => (
          <span
            key={i}
            className={[
              "block w-[3px] transition-[height,background-color] duration-75",
              i / 18 < level ? "bg-sage-dim" : "bg-hairline",
            ].join(" ")}
            style={{ height: 4 + (i % 3) * 4 + (i / 18 < level ? 8 : 0) }}
          />
        ))}
      </div>

      <span className="micro whitespace-nowrap text-faint">STOP TO TRANSCRIBE</span>
    </div>
  );
}
