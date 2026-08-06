/**
 * Compile the on-device language model helper.
 *
 * The public build has no AI at all today: `generate_brief` calls Bedrock and
 * lives behind the `assistant` feature. This helper is the answer that costs
 * nothing to run — macOS 26's own model, driven from Swift because
 * `FoundationModels` has no C surface to bind against. See the top of
 * brain-helper/main.swift.
 *
 * Recompiles only when main.swift is newer than the binary, so a warm tree pays
 * nothing.
 */
import { execFileSync } from "node:child_process";
import { statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const sources = ["main.swift"].map((f) => join(root, "brain-helper", f));
const src = sources[0];
const out = join(root, "brain-helper", "voicedumps-brain");

if (process.platform !== "darwin") {
  // There is no on-device Apple model to reach anywhere else, and nothing that
  // would call this.
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
  console.error(`[brain] missing ${src} — cannot build the on-device model helper.`);
  process.exit(1);
}

const outTime = mtime(out);
if (outTime !== null && outTime >= srcTime) {
  process.exit(0); // up to date
}

// The same floor as the other two helpers, even though everything this one
// actually does needs macOS 26. That is the point: with the deployment target
// down here the linker weak-links FoundationModels, so the binary still *loads*
// on an older Mac and can answer "this Mac is too old" in its own words.
// Building at 26 would produce a helper that dyld refuses to start, and the app
// would report a missing helper instead of an unsupported one.
const TARGET = "arm64-apple-macos11.0";

console.log("[brain] compiling on-device model helper…");
try {
  execFileSync("swiftc", ["-O", "-target", TARGET, ...sources, "-o", out], { stdio: "inherit" });
  console.log("[brain] built brain-helper/voicedumps-brain");
} catch (err) {
  // Don't take the dev server down: everything except the overview still works,
  // and the failure is printed loudly enough to act on.
  console.error(`[brain] build failed — overviews will be unavailable. ${err.message}`);
  process.exit(1);
}
