/**
 * Compile the native dictation-overlay helper.
 *
 * The pill that floats over other apps is a separate Swift accessory process —
 * a Tauri webview window cannot sit above another app's full-screen Space, but
 * an NSPanel can. That binary used to be compiled by hand and left in the tree,
 * which meant a fresh checkout silently had no dictation overlay at all: the
 * app starts fine, the globe key appears to do nothing, and there's no error
 * because `prepare_overlay` just can't find the file.
 *
 * Wiring it into the npm scripts makes it impossible to forget. Recompiles only
 * when main.swift is newer than the binary, so it costs nothing on a warm tree.
 */
import { execFileSync } from "node:child_process";
import { statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const sources = ["main.swift", "pdf.swift"].map((f) => join(root, "overlay-helper", f));
const src = sources[0];
const out = join(root, "overlay-helper", "voicedumps-overlay");

if (process.platform !== "darwin") {
  // The globe key and the NSPanel are both macOS-only; elsewhere there's
  // nothing to build and nothing that would use it.
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
  console.error(`[overlay] missing ${src} — cannot build the dictation overlay.`);
  process.exit(1);
}

const outTime = mtime(out);
if (outTime !== null && outTime >= srcTime) {
  process.exit(0); // up to date
}

console.log("[overlay] compiling dictation overlay…");
try {
  execFileSync("swiftc", ["-O", ...sources, "-o", out], { stdio: "inherit" });
  console.log("[overlay] built overlay-helper/voicedumps-overlay");
} catch (err) {
  // Don't take the whole dev server down: everything except the floating pill
  // still works, and the failure is printed loudly enough to act on.
  console.error(`[overlay] build failed — dictation overlay will not appear. ${err.message}`);
  process.exit(1);
}
