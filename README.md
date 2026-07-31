# VoiceDumps

Local speech to text for macOS. Hold the globe key and dictate into any app,
record from the microphone, or drop in an audio or video file and read the
transcript back.

Everything runs on your Mac. There is no account, no API key, nothing to
configure, and no network call — the speech models ship inside the app, so it
works offline the first time you open it.

## Install

Download the DMG from [Releases](../../releases/latest), drag **VoiceDumps** to
Applications, and open it.

The app is signed, but not with a paid Apple Developer certificate, so macOS
will refuse the first launch and say it cannot verify the developer. To get past
it: open **System Settings → Privacy & Security**, scroll down, and click
**Open Anyway**. Then two permissions:

- **Microphone** — prompted the first time you record. Required for dictation.
- **Accessibility** — **System Settings → Privacy & Security → Accessibility**.
  Required for the globe key, because watching for a modifier key means
  installing an event tap. macOS never grants this automatically; no app can.

Apple Silicon only. Building for Intel means recompiling whisper.cpp for
`x86_64`.

## What it does

- **Globe-key dictation** anywhere — hold, speak, release, and the text lands in
  whatever app you were in. A floating pill shows the level while you talk.
- **Microphone recording** in the app, with a waveform.
- **Drop a file** — mp3, m4a, wav, flac, ogg, mp4, mov. Video included; the
  audio track is pulled out and the rest ignored.
- **A reader, not a text dump** — transcripts are broken into paragraphs, with
  timestamps hung in the margin that seek when you click them, and can be edited
  in place. Reading and listening are treated as different things: press play
  and it follows the spoken word, dimming the rest; leave it paused and you get
  an ordinary page at one size and full contrast, with nothing chasing you.
  `⌘F` finds within the open transcript, `↑`/`↓` step by paragraph, `Space`
  plays and pauses, `←`/`→` skip five seconds.
- **Export** to PDF, Markdown or plain text. The PDF is typeset, not a printed
  screenshot.
- **Search** across everything you have transcribed.
- **Light and dark**, remembered between launches.
- Lives in the menu bar. Closing the window keeps the globe key working;
  quitting from the tray menu stops it.

Two models ship inside the app and it picks one on launch: **medium** (514 MB)
on a Mac with 16 GB of memory or more, **small** (181 MB) below that. Force
either with `VOICEDUMPS_MODEL_SIZE=small` or `=medium`.

## Build it yourself

You need Xcode Command Line Tools, [CMake](https://cmake.org) (whisper.cpp
builds with it), [Node](https://nodejs.org) 20+, and [Rust](https://rustup.rs).

```bash
git clone https://github.com/heynaavi/voiceDump.git
cd voiceDump
npm install
npm run models      # ~695 MB, from Hugging Face, into ./models
npm run dev         # or: npm run bundle
```

`npm run models` is a separate step because the weights are too large for git.
`npm run dev` runs the app against a live Vite server; `npm run bundle` produces
a signed `.app` and `.dmg` under `src-tauri/target/release/bundle/`.

## How it works

| Layer | Language | Role |
| --- | --- | --- |
| `src/` | React + Tailwind | The window: drag and drop, the reader, history, theming |
| `src-tauri/src/` | Rust (Tauri v2) | Commands, SQLite history, the transcription engine, the event tap |
| `overlay-helper/` | Swift | The floating dictation pill, and the PDF typesetter |

Three decisions explain most of the rest:

**Transcription is in-process.** `engine.rs` calls whisper.cpp directly through
`whisper-rs` (Metal-accelerated) and decodes audio with `symphonia`, a pure-Rust
decoder. No Python, no `ffmpeg`, no subprocess. That is what makes the app a
single download that works offline.

**The dictation pill is a separate process.** A Tauri webview window cannot be
placed above another app's full-screen Space — no window level or collection
behaviour achieves it. A native `NSPanel` can, so the pill lives in a small
Swift accessory binary the app drives over a pipe. That binary also typesets PDF
exports with CoreText, since it already has the fonts and the text engine.

**The model is loaded once and held.** Loading takes a second or two, so
dictation warms it the moment you press the key rather than after you release
it, and it is dropped explicitly on quit — leaving a live whisper context to the
C runtime's exit handlers trips an assertion inside ggml's Metal backend, and
the app "crashes" on an ordinary quit. See `exits_cleanly_with_a_model_loaded`
in `engine.rs`.

## Tests

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

Two of them need real inputs and skip without them:

```bash
TEST_AUDIO=/path/to/clip.wav VOICEDUMPS_MODEL_DIR=$PWD/models \
  cargo test --manifest-path src-tauri/Cargo.toml
```

## Contributing

Issues and pull requests are welcome. The comments in this codebase explain
*why* rather than *what* — please match that, especially where something looks
odd, because it is usually odd for a reason worth writing down.

## Licence

MIT — see [LICENSE](LICENSE).

Built on [whisper.cpp](https://github.com/ggerganov/whisper.cpp) and
[whisper-rs](https://github.com/tazz4843/whisper-rs), with models from
[ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp) on Hugging
Face. Audio decoding by [Symphonia](https://github.com/pdeljanov/Symphonia).
