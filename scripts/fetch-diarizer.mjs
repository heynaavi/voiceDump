// The speaker diarizer's native pieces, fetched rather than committed.
//
// Same shape as the Swift helpers beside it: a build step produces a binary the
// bundle carries, and git never sees it. Unlike those, this one is downloaded
// rather than compiled — sherpa-onnx is a large C++ project and building it from
// source would make a clean checkout take an hour to get to a running app.
//
// The models are NOT here. They are data, so they are fetched on demand at
// runtime by `diarize::fetch` and live in the data directory, where replacing
// the .app does not cost a re-download. What has to be in the bundle is only the
// part macOS refuses to load from anywhere else: under the hardened runtime,
// library validation rejects a dylib that was not present at signing time.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(root, "diarize-helper");

// Pinned. onnxruntime 1.17.1 is 4 MB smaller and scores 80.0% where this scores
// 93.2% on the same fixture at the same threshold — measured, not assumed.
const VERSION = "v1.13.7";
const ARCHIVE = `sherpa-onnx-${VERSION}-osx-arm64-shared-no-tts.tar.bz2`;
const URL = `https://github.com/k2-fsa/sherpa-onnx/releases/download/${VERSION}/${ARCHIVE}`;
const SHA256 = "6a78081a617727ebb91a6449aaa9d98fa556272f8f7600a7c2308c9f100e2953";

// Only what the diarizer actually opens. The release carries recognisers,
// punctuation and VAD models' runtimes too, and none of them are wanted here.
const WANTED = [
  ["bin/sherpa-onnx-offline-speaker-diarization", "voicedumps-diarize"],
  ["lib/libonnxruntime.dylib", "libonnxruntime.dylib"],
  ["lib/libsherpa-onnx-c-api.dylib", "libsherpa-onnx-c-api.dylib"],
];

if (WANTED.every(([, name]) => existsSync(join(out, name)))) {
  process.exit(0);
}

mkdirSync(out, { recursive: true });
const tarball = join(out, ARCHIVE);
console.log(`fetching the diarizer helper (${VERSION})…`);
execFileSync("/usr/bin/curl", ["--fail", "--location", "--silent", "--show-error",
  "--retry", "3", "--output", tarball, URL], { stdio: "inherit" });

const got = createHash("sha256").update(readFileSync(tarball)).digest("hex");
if (got !== SHA256) {
  rmSync(tarball, { force: true });
  throw new Error(`diarizer archive checksum mismatch: ${got}`);
}

const stem = ARCHIVE.replace(/\.tar\.bz2$/, "");
execFileSync("/usr/bin/tar", ["-xjf", tarball, "-C", out,
  ...WANTED.map(([p]) => `${stem}/${p}`)], { stdio: "inherit" });

for (const [path, name] of WANTED) {
  execFileSync("/bin/mv", [join(out, stem, path), join(out, name)]);
}
rmSync(join(out, stem), { recursive: true, force: true });
rmSync(tarball, { force: true });

// The helper looks for its libraries beside itself rather than in the build
// machine's directory layout, which is where they will be once Tauri has copied
// all three into Resources.
// Look for the libraries beside the binary rather than in the build machine's
// layout — which is where they will be once Tauri has copied all three into
// Resources.
execFileSync("/usr/bin/install_name_tool", ["-add_rpath", "@executable_path",
  join(out, "voicedumps-diarize")], { stdio: "inherit" });

// Re-sign, because rewriting the load commands invalidated the signature it
// arrived with — and on Apple Silicon an invalid signature does not warn, it
// refuses to execute. Ad-hoc, matching how the app itself is signed.
execFileSync("/usr/bin/codesign", ["--force", "--sign", "-",
  join(out, "voicedumps-diarize")], { stdio: "inherit" });
console.log(`  ${WANTED.length} files in diarize-helper/`);
