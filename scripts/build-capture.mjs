/**
 * Compile the system-audio capture helper.
 *
 * Meeting capture needs the far side of the call — whatever the Mac is playing —
 * and that comes from a CoreAudio process tap, which is driven from Swift for
 * the reasons written at the top of capture-helper/main.swift. Like the
 * dictation overlay, it is a separate binary the app spawns, so it needs a build
 * step that cannot be forgotten: without it the app runs fine, meeting capture
 * reports that the helper is missing, and nothing else explains why.
 *
 * Recompiles only when main.swift is newer than the binary, so a warm tree pays
 * nothing.
 */
import { execFileSync } from "node:child_process";
import { statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const sources = ["main.swift"].map((f) => join(root, "capture-helper", f));
const src = sources[0];
const out = join(root, "capture-helper", "voicedumps-capture");

if (process.platform !== "darwin") {
  // CoreAudio taps are macOS-only; elsewhere there is nothing to build and
  // nothing that would use it.
  process.exit(0);
}

const mtime = (p) => {
  try {
    return statSync(p).mtimeMs;
  } catch {
    return null;
  }
};

const srcTime = Math.max(...sources.map((f) => mtime(f) ?? 0)) || null;
if (srcTime === null) {
  console.error(`[capture] missing ${src} — cannot build the capture helper.`);
  process.exit(1);
}

const outTime = mtime(out);
if (outTime !== null && outTime >= srcTime) {
  process.exit(0); // up to date
}

// Same floor as the overlay, and for the same reason: pinning the deployment
// target is what makes the compiler reject an API newer than the oldest Mac we
// support, instead of silently shipping a binary that dies on launch there.
//
// 11.0 rather than 14.4 even though the tap API is 14.4-only. The helper is
// meant to *run* on an older Mac and say so — `runCapture()` is annotated
// `@available(macOS 14.4, *)` and the entry point checks at runtime — which
// only type-checks if the target is low enough to make that check meaningful.
const TARGET = "arm64-apple-macos11.0";

console.log("[capture] compiling system-audio capture helper…");
try {
  execFileSync("swiftc", ["-O", "-target", TARGET, ...sources, "-o", out], { stdio: "inherit" });
  console.log("[capture] built capture-helper/voicedumps-capture");
} catch (err) {
  // Don't take the dev server down: everything except meeting capture still
  // works, and the failure is printed loudly enough to act on.
  console.error(`[capture] build failed — meeting capture will be unavailable. ${err.message}`);
  process.exit(1);
}
