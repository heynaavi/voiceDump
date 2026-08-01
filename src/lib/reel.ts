/**
 * The word cloud as a short portrait video.
 *
 * Same card, same drawing code as the PNG — `share.paint` takes a `Reveal` and
 * this animates it with GSAP instead of passing `FULL`. Keeping one painter
 * means the still and the reel cannot drift apart, and the last frame of the
 * video is the PNG.
 *
 * Recorded rather than encoded frame by frame: `canvas.captureStream()` plus
 * `MediaRecorder` is the only encoder available without shipping one, and it
 * keeps the export offline, which is the whole point of the app. The cost is
 * that recording happens in real time — a seven-second reel takes seven
 * seconds — and that the container is whatever the webview will encode.
 *
 * **On the container.** Instagram accepts MP4 and MOV, and rejects WebM.
 * WKWebView encodes H.264 in MP4; Chromium encodes VP8/VP9 in WebM. So the
 * type is probed rather than assumed, and the caller is told which one it got —
 * a WebM that silently fails to upload is worse than a refusal.
 */

import gsap from "gsap";

import { board, paint, type CardData, type Reveal } from "./share";

/** Frames per second. 30 is the floor for anything that looks deliberate. */
const FPS = 30;

/** In preference order: what Instagram takes first, what we can make second. */
const TYPES = [
  "video/mp4;codecs=avc1.42E01E",
  "video/mp4;codecs=h264",
  "video/mp4",
  "video/webm;codecs=vp9",
  "video/webm;codecs=vp8",
  "video/webm",
];

export type Reel = {
  bytes: Uint8Array;
  /** "mp4" or "webm" — the caller names the file and warns if it isn't mp4. */
  extension: string;
  mime: string;
};

/** What this webview can actually encode, best first. `null` if nothing. */
export function bestType(): string | null {
  if (typeof MediaRecorder === "undefined") return null;
  return TYPES.find((t) => MediaRecorder.isTypeSupported(t)) ?? null;
}

/**
 * Animate the card and record it.
 *
 * The timeline drives a plain `Reveal` object; every tick repaints the canvas,
 * and the canvas is what the recorder is watching. `onUpdate` rather than a
 * ticker callback so the paint is bound to the timeline's own clock.
 */
export async function renderReel(
  data: CardData,
  onProgress?: (fraction: number) => void,
): Promise<Reel> {
  const mime = bestType();
  if (!mime) throw new Error("this webview cannot record video");

  const { canvas, ctx } = await board();

  // First frame before the recorder starts, so the video never opens on a
  // transparent flash while the timeline is still being built.
  const at: Reveal = { grain: 0, header: 0, words: 0, footer: 0, brand: 0 };
  paint(ctx, data, at);

  const stream = canvas.captureStream(FPS);
  const recorder = new MediaRecorder(stream, {
    mimeType: mime,
    videoBitsPerSecond: 12_000_000,
  });
  const chunks: BlobPart[] = [];
  recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);

  const done = new Promise<Blob>((resolve, reject) => {
    recorder.onstop = () => resolve(new Blob(chunks, { type: mime }));
    recorder.onerror = () => reject(new Error("the recorder stopped early"));
  });

  recorder.start();

  const words = Math.min(data.words.length, 25);
  // Paused, and advanced by wall clock below rather than by GSAP's own ticker.
  //
  // The ticker runs on requestAnimationFrame, which the OS stops delivering
  // when the window isn't being composited — switch apps mid-render and the
  // timeline freezes while the recorder keeps rolling, so you get a stalled
  // frame held for however long you were away. Seeking to elapsed real time
  // means an unfocused window costs dropped frames instead of a broken file,
  // and the render always terminates.
  const tl = gsap.timeline({ paused: true });

  // The page arrives before anything is written on it.
  tl.to(at, { grain: 1, duration: 0.5, ease: "power1.out" }, 0)
    .to(at, { header: 1, duration: 0.7, ease: "power2.out" }, 0.25)
    // The words are the point, so they get the middle and most of the runtime.
    // `none` because each word has its own ease inside `paint` — easing the
    // counter as well would bunch the arrivals at both ends.
    .to(at, { words, duration: Math.max(1.8, words * 0.085), ease: "none" }, 0.95)
    .to(at, { footer: 1, duration: 0.6, ease: "power2.out" }, ">-0.1")
    // Branding last and quietly, assembling cell by cell like the app's own
    // mark does — a logo that announces itself over the content gets cropped.
    .to(at, { brand: 1, duration: 0.9, ease: "power1.out" }, ">-0.2")
    // A beat on the finished card, which is also the still image.
    .to({}, { duration: 1.2 });

  const total = tl.duration();
  const started = performance.now();
  await new Promise<void>((resolve) => {
    const tick = setInterval(() => {
      const t = (performance.now() - started) / 1000;
      tl.time(Math.min(t, total));
      paint(ctx, data, at);
      onProgress?.(Math.min(1, t / total));
      if (t >= total) {
        clearInterval(tick);
        resolve();
      }
    }, 1000 / FPS);
  });

  // A moment at the end so the last frames reach the encoder before it closes.
  await new Promise((r) => setTimeout(r, 250));
  recorder.stop();
  stream.getTracks().forEach((t) => t.stop());

  const blob = await done;
  return {
    bytes: new Uint8Array(await blob.arrayBuffer()),
    extension: mime.startsWith("video/mp4") ? "mp4" : "webm",
    mime,
  };
}
