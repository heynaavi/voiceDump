<div align="center">

<img src="docs/media/icon.png" width="88" alt="VoiceDumps" />

# VoiceDumps

**Hold the globe key. Talk. The words appear where your cursor already was.**

Local speech-to-text for macOS — dictation, meeting recording, and a library
you can ask questions of. No account, no API key, nothing uploaded: the speech
models are downloaded once and run on your own Mac.

[![Latest release](https://img.shields.io/github/v/release/heynaavi/voiceDump?label=release&color=1e2618)](https://github.com/heynaavi/voiceDump/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/heynaavi/voiceDump/total?label=downloads&color=8fb07c)](https://github.com/heynaavi/voiceDump/releases)
![Platform](https://img.shields.io/badge/macOS%2011%2B-Apple%20Silicon-1e2618)
![Offline](https://img.shields.io/badge/works-offline-8fb07c)
![Price](https://img.shields.io/badge/price-free-8fb07c)

<img src="docs/media/dictation.gif" width="820" alt="Holding the globe key in a mail composer: a floating pill shows the live voice level, and the spoken words arrive at the cursor." />

<sub>**[Watch with sound →](docs/media/launch-wide.mp4)** &nbsp;·&nbsp; [Portrait cut](docs/media/launch-portrait.mp4)</sub>

</div>

---

## What it does

You are already typing somewhere — Slack, Mail, a terminal, a code comment. Hold
the globe key, say the sentence, let go. The text lands at the cursor in the app
you were already in. Nothing switches, nothing is pasted by hand.

Not everyone has a globe key free, so it is only the default: SETTINGS will
record whatever chord you would rather hold — ⌃⌥, ⇧⌘, whatever your hands
already know. Modifiers only, and for a reason worth stating plainly — the
keyboard is *watched*, never taken over, so anything in the chord also reaches
the app underneath. ⌃⌥ types nothing anywhere; ⌃⌥D would arrive in your editor
mid-sentence.

Then there is the other half: drop in audio or video and get a readable
transcript with real timestamps, word-level follow-along during playback, inline
editing that keeps the timings, full-text search across everything you have ever
dictated, and export to typeset PDF, Markdown or plain text — or to Markdown
with a start and end time on every single word, for whatever you want to line up
against the audio next.

**Calls, both sides.** Start a recording and your microphone and whatever the
Mac is playing are captured as two separate tracks, so the transcript knows who
said what without guessing at voices. No bot joins the meeting — Zoom, Meet,
Teams and a phone on speaker all work the same way, because it is the audio
that is captured, not the app.

**And a way back if it gets one wrong.** Every recording keeps its own copy of
the audio, so TRANSCRIBE AGAIN reads it a second time and replaces the words.
Speech recognition does not fail loudly — it degrades, and the person who can
tell is the one who was in the room. It asks first: a re-read discards any edits
you made to the text, and a saved meeting keeps one mixed track, so the words
come back but the You / Others labels do not.

**Names, overviews and answers.** On macOS 26 with Apple Intelligence switched
on, every recording gets a short real title, an overview with its key points and
action items, and a note of the people and projects it was about. Then ASK YOUR
NOTES puts a question to the whole library — *what did we decide about pricing*,
*what are my action items* — and answers from the recordings that bear on it,
citing the ones it used so you can open them and check. You can follow up in
plain language: "write that as a paragraph", "make it shorter", "as bullets".

That half runs on Apple's on-device model, so it holds the same promise as the
rest: nothing is uploaded, and there is still nothing to sign in to. Without
Apple Intelligence — an older macOS, an ineligible Mac, or simply switched off —
everything else works exactly as it always did, and ASK still finds and shows
you the recordings that match.

## How fast, exactly

Numbers invite scepticism, so this repo ships the benchmark rather than the
claim:

```bash
scripts/bench.sh                        # 12s of synthesised speech
scripts/bench.sh path/to/your.wav       # or your own audio
VOICEDUMPS_MODEL_SIZE=small scripts/bench.sh
```

**Apple M1 Pro, 16 GB, macOS 27, 8 threads. `small` model. Time from the moment
you stop speaking to text on the clipboard.**

| You spoke for | Median | Worst of 3 |
| --- | --- | --- |
| 1.9 s | **377 ms** | 415 ms |
| 5.8 s | **423 ms** | 429 ms |
| 12.0 s | **898 ms** | 974 ms |

Note that the first two rows are the same number. whisper processes a fixed
30-second window, so everything shorter than that costs what the window costs —
a two-second "ship it Thursday" and a six-second sentence land in the same ~400 ms.

<details>
<summary>Full engine figures, both models, 12 s of speech</summary>

<br>

| | `small` | `medium` |
| --- | --- | --- |
| Model load | 0.20 s | 0.43 s |
| Transcribe | 0.90 s | 2.29 s |
| Speed vs realtime | 13× | 5× |
| Resident memory | 260 MB | 601 MB |

</details>

Three honest footnotes, because the table is flattering enough without them:

- **This is one laptop, not a spec sheet.** Thermal state, threads and what else
  is running all move it. Run `scripts/bench.sh` and see what your machine does.
- **The timings exclude model load on purpose, and that is legitimate.** Dictation
  calls `engine::warm` the instant the key goes *down*
  ([`dictation.rs`](src-tauri/src/dictation.rs)), so the load overlaps with you
  still speaking. Speak for longer than half a second and you never pay it.
- **A genuinely first-ever load is much worse.** Reading the 539 MB medium file
  when it is nowhere in the OS page cache measured **9.7 s** here. Every
  subsequent load was ~0.4 s. First launch after install is the slow one.

Audio decoding is 2 ms of the budget — `symphonia`, in-process, no `ffmpeg` in
the transcription path at all.

**Long recordings.** A dictation is seconds; a meeting is not, and the question
that actually gets asked is where this stops working. Measured on the same
laptop, `medium`, a **3 h 14 m** recording: **13 min 12 s** to transcribe —
about 15× faster than listening to it — for 14,198 words, with the last fifth
of the recording denser than the first. Memory holds flat for the whole decode
rather than climbing as it goes. What scales with length is the audio itself,
held as one buffer at **230 MB per hour**, on top of about 1.1 GB for the model
and its decoder. Nothing here is a wall you would meet in a day's work; a
recording long enough to matter would have to run most of a working day.

## How it compares

Cloud dictation is the norm, and [Wispr Flow](https://wisprflow.ai/pricing) is
the best-known example — **$15/month or $144/year**, free tier capped at
[2,000 words per week](https://www.getvoibe.com/resources/wispr-flow-pricing/).

They publish their latency, which makes an honest comparison possible. Their
inference provider's case study, quoting Wispr Flow's CTO, states the pipeline
"runs end-to-end in **under 700 milliseconds**", and is explicit that this is a
**p99** target rather than a median
([Baseten](https://www.baseten.co/resources/customers/wispr-flow/)).

Against that published figure, for a normal dictation utterance:

| | Latency after you stop speaking |
| --- | --- |
| Wispr Flow (published, p99, cloud) | under 700 ms |
| **VoiceDumps** (measured, worst of 3, local) | **429 ms** |

That is roughly **39% faster** — and the comparison is deliberately stacked
against us: their p99 versus our worst case, on the `small` model.

**Where that number stops being true.** Our latency scales with how long you
spoke; theirs is a mostly fixed pipeline cost because it transcribes while you
talk. Past roughly ten seconds of speech we lose that race — 12 seconds took us
~900 ms. The win is on short utterances, which is what dictation mostly is.

**And what their 700 ms buys that ours does not.** That budget includes a
fine-tuned Llama pass that reformats and contextualises the transcript, ~250 ms
of it by their own account. We do not do that. We hand you what you said.

The rest is architecture, and it does not move:

| | VoiceDumps | Cloud dictation |
| --- | --- | --- |
| Price | Free | Typically $12–15/month |
| Word limits | None | Free tiers are usually capped |
| Works on a plane | Yes | No |
| Where your voice goes | Stays on the disk it recorded to | Uploaded for inference |
| Account required | No | Yes |
| Network round trip | **None** | Once per utterance |
| Can have an outage | No | [Yes](https://www.getvoibe.com/resources/wispr-flow-outage-june-2026/) |

A cloud service cannot start before your audio arrives. From this machine, *just
the TLS handshake* — before a byte of audio moves or a single token is generated —
measured **19 ms** to an edge-cached endpoint and **544–564 ms** to two regional
speech APIs. Local pays zero of that, every time, and does not get slower on
hotel wifi.

Where cloud genuinely wins: a datacentre GPU running a larger model beats `small`
on hard audio — thick accents, crosstalk, bad microphones — and it will format
your text for you. If that is your workload, run `medium` and take 5× realtime
instead of 13×.

## Inside

<table>
<tr>
<td width="50%">

**Dictation, over any app**

<img src="docs/media/overlay-listening.png" width="100%" alt="The floating pill showing live voice level over a mail composer" />

The pill is its own accessory process
([`overlay-helper/main.swift`](overlay-helper/main.swift)) — a Tauri webview
cannot float over another app's full-screen Space, an `NSPanel` can. The level
bars are the real signal: CoreAudio RMS, metered every 50 ms. Nothing reaches
your document until you let go, and then it arrives in one piece.

</td>
<td width="50%">

**Transcripts you can actually read**

<img src="docs/media/transcript.png" width="100%" alt="A transcript with hung timestamps and the current word highlighted" />

Whisper emits 5–10 second fragments, which are miserable to read.
`build_paragraphs` merges them on real pauses after completed sentences. Word
timings survive, so playback follows along and editing keeps the sync.

</td>
</tr>
<tr>
<td width="50%">

**It stays on your machine**

<img src="docs/media/privacy.png" width="100%" alt="No account, no API key, no upload" />

The app downloads its speech models once, on first run, and never fetches
anything again — no account, no API key, and no service to call, so there is no
key to leak. Your audio and your notes are never sent anywhere: transcription
runs in-process on your own machine, before and after that one download.

</td>
<td width="50%">

**What the history knows**

<img src="docs/media/insights.png" width="100%" alt="Insights: words dictated, words per minute, where you dictate, and an activity grid" />

Words, pace, which apps you dictate into, and an activity grid — computed
locally in [`analytics.rs`](src-tauri/src/analytics.rs). It refuses to print a
words-per-minute figure from too thin a sample rather than guessing.

</td>
</tr>
<tr>
<td width="50%">

**Both sides of a call, kept apart**

Your microphone and the Mac's own output are recorded as two streams and
transcribed separately, so attribution is a fact about which track a sentence
arrived on rather than a guess about a voice. The far side is taken with a
CoreAudio process tap in a helper
([`capture-helper/main.swift`](capture-helper/main.swift)); nothing is injected
into the meeting app and no participant sees a bot.

</td>
<td width="50%">

**A question, not a search box**

ASK reads the recordings that bear on what you asked and answers from them, with
citations you can open. Retrieval runs two ways at once — full-text search for
what notes *say*, a knowledge graph for what they are *about* — because each
fails where the other works. The answer is generated under a schema rather than
a prompt asking nicely for one, which is what stops a 3-billion-parameter model
replying with a tool call instead of a sentence.

</td>
</tr>
</table>

<sub>Images are stills from the launch film in `docs/media`, which rebuilds the
product's own UI at 1080p rather than screenshotting it.</sub>

## Install

Apple Silicon, macOS 11 or later, about 5 MB.

```bash
curl -fsSL https://raw.githubusercontent.com/heynaavi/voiceDump/main/scripts/install.sh | bash
```

That fetches the latest DMG, checks its signature, copies the app to
`/Applications`, and clears the download quarantine flag so macOS does not
refuse to open it — see below for why that flag is the problem. No `sudo`,
nothing written outside `/Applications`, and
[the script](scripts/install.sh) is forty lines you can read first, which you
should: piping anything into `bash` deserves that much.

Or grab the DMG from
[**Releases**](https://github.com/heynaavi/voiceDump/releases/latest) and drag
it to Applications the usual way — then read the next section, because macOS
will stop you.

On first launch it downloads the speech models it needs — 729 MB on a machine
with 16 GB of memory or more, 190 MB below that, where the larger model would
only swap. They are saved beside your notes rather than inside the app, so they
survive every update and you are asked once. After that the app never fetches
anything unless you click the version number to check for a release.

### macOS will say it cannot verify the app

If you installed with the command above, it will not — that is the whole reason
the command exists. If you dragged the DMG across yourself, it will, and there
is no way around it that does not cost $99 a year.

> **"VoiceDumps" Not Opened** — Apple could not verify "VoiceDumps" is free of
> malware that may harm your Mac or compromise your privacy.

That dialog does not mean anything was found. It means the app is not
*notarized*: not sent to Apple for scanning and signed with a paid Developer ID
certificate. This is a one-person open-source project, so it is signed with an
ad-hoc signature instead, and macOS flags anything downloaded from a browser
that it cannot trace to a paid account. Every unsigned Mac app you have ever
installed showed you this.

The build is reproducible — clone the repo, run `npm run build:lite`, and
compare. Do not take the dialog's advice and click **Move to Bin**; either:

```bash
xattr -dr com.apple.quarantine /Applications/VoiceDumps.app
```

or open it once through **System Settings → Privacy & Security**, scroll to the
message about VoiceDumps being blocked, and click **Open Anyway**. Either way it
is asked once, not every launch. Recent macOS versions have narrowed the old
Control-click → Open shortcut, so if that does not offer you an Open button, use
one of the two above.

Two permissions on first run, both unavoidable for what it does:

| Permission | Why |
| --- | --- |
| **Accessibility** | The globe key is a *modifier*, not a keycode. It emits no key-down event, so no shortcut API can see it — reading it needs a `CGEventTap`. |
| **Microphone** | To hear you. |

Also set **System Settings → Keyboard → "Press 🌐 to:" → Do Nothing**, or macOS
opens the emoji picker underneath the app.

## Build it yourself

Needs Node, Rust and CMake (whisper.cpp builds with it). No Python and no
`ffmpeg` for the transcription path.

```bash
npm install
npm run models         # ~730 MB of ggml weights into ./models
npm run tauri dev      # or: npm run build:lite  → .app + .dmg
```

<details>
<summary><b>The models, and which one you get</b></summary>

<br>

Two quantised multilingual ggml models, fetched on first run into
`~/Library/Application Support/dev.heynaavi.voicedump/models` and kept there
across updates:

| | File | On disk | Resident |
| --- | --- | --- | --- |
| `small` | `ggml-small-q5_1.bin` | 190 MB | 260 MB |
| `medium` | `ggml-medium-q5_0.bin` | 539 MB | 601 MB |

Chosen at launch by memory, not chip generation: **16 GB or more gets `medium`**,
below that `small`. A base M1 with 8 GB is already juggling a browser and an
editor, and making the whole machine swap to win a little accuracy is a bad
trade. Force either with `VOICEDUMPS_MODEL_SIZE=small|medium`.

Multilingual rather than the `.en` variants because the app already handles
non-English notes; q5 because the accuracy difference is inaudible next to
halving the download.

</details>

<details>
<summary><b>Architecture</b></summary>

<br>

| Layer | Path | Role |
| --- | --- | --- |
| UI | `src/` | React + Tailwind. Reader, history, insights, drag-drop. |
| Shell | `src-tauri/` | Tauri v2. Owns the SQLite history, the globe-key tap and the engine. |
| Engine | `src-tauri/src/engine.rs` | whisper.cpp via `whisper-rs`, Metal-accelerated. In-process. |
| Overlay | `overlay-helper/` | Swift `NSPanel` for the dictation pill and PDF typesetting. |

Transcription used to run in a Python sidecar on MLX. That worked but could
never ship: it needed a 1 GB virtualenv, the `mlx` stack, and `ffmpeg` on the
user's `PATH`. "Download the app and run it" is not possible on those terms, so
the engine moved in-process and the weights became a one-time download the app
manages itself.

**The model stays warm.** Loading costs real time, so one `WhisperContext` is
held between jobs and back-to-back dictations pay nothing for it. Two callers
racing — a warm-up and the transcription it was warming for — would each build a
context and spike memory, so the lock is deliberately held across the load.

The context is released on quit, and that teardown is load-bearing rather than
tidy: `-[NSApplication terminate:]` calls `exit()`, which drops nothing Rust
owns, so a live context was still holding Metal residency sets when ggml-metal
asserted they were all released — `abort()`, and macOS reporting a crash on an
ordinary quit. There is a regression test that runs the exit in a child process,
because the behaviour under test is what happens *during* exit.

**The live preview is off by default** (SETTINGS, at the foot of the sidebar,
under DICTATION). Turned on, the overlay drafts your words while you are still speaking.
Whisper has no incremental decode, so it transcribes *forward* — a cursor marks
what has been read, each pass takes only what is new, and chunks are cut at the
quietest moment in their tail so boundaries land between words rather than inside
them. Pass cost is bounded by the chunk rather than by how long you have been
talking, which is what keeps the lag flat.

It ships off because of what it costs to read. The preview runs on `small` while
the real transcription stays on `medium`, and it is thrown away the moment the
authoritative pass lands — so what you watch appear is a faster, worse guess at a
sentence you already know you said, and it will quietly change. Being shown a
mangled version of your own words invites you to correct something that was never
going to be pasted. It is genuinely useful as reassurance that the microphone is
working; it is not a draft, and defaulting it on presented it as one.

**Nothing is inserted progressively,** either way. Doing so would mean
*retracting* text when the transcript revises itself — firing backspaces into
whatever you were writing — so the preview stays in the panel and the paste
happens once, from one clean read of the whole recording.

**Text is pasted, not typed.** Synthetic keystrokes are slow and get mangled by
autocorrect and input methods, so the transcript goes to the clipboard and the
app synthesises ⌘V.

</details>

<details>
<summary><b>Your data</b></summary>

<br>

Transcripts live in SQLite at
`~/Library/Application Support/dev.heynaavi.voicedump/voicedumps.db`, with an FTS5
index behind the search box.

Audio is **copied** into `…/media/`, organised by month, so playback survives you
moving or deleting the original — the store keeps both the archived copy and the
path it came from. The copy is normalised to mono AAC so that everything is
playable by the same `<audio>` element: `symphonia` decodes, and CoreAudio's own
`/usr/bin/afconvert` encodes. No `ffmpeg`, here or anywhere else.

Dictation records to a scratch WAV in `…/dictation/`, which is deleted the moment
the library has its copy. A capture that never reaches that point — a failed
transcription, a crash mid-recording — used to be left behind forever; since
0.4.1 anything still there after a day is swept at launch. Nothing that became a
transcript is ever touched by the sweep, because it is already gone by then.

Nothing is transmitted. There is no telemetry, no crash reporting and no update
check in this build.

**Memory is given back.** The model loads on first use, not at launch, and is
released after five idle minutes — measured on an M1 Pro, that returns 512 MB of
the 578 MB `medium` costs. This matters because the app lives in the menu bar:
closing the window deliberately keeps the globe key alive, so "not quitting" is
the normal state, and a model held until quit is a model held all day.

Reloading costs ~560 ms and is free in practice, because dictation warms the
model on key *down* — the reload happens while you are still speaking. Set
`VOICEDUMPS_IDLE_UNLOAD_SECS=0` if you would rather spend the memory.

</details>

<details>
<summary><b>Tuning</b></summary>

<br>

| Variable | Meaning |
| --- | --- |
| `VOICEDUMPS_MODEL_SIZE` | `small` or `medium`, overriding the memory-based choice |
| `VOICEDUMPS_MODEL_DIR` | Where to find the weights, ahead of the downloaded copy |
| `VOICEDUMPS_IDLE_UNLOAD_SECS` | Seconds of disuse before the model is released. Default `300`; `0` keeps it loaded forever |
| `TEST_AUDIO` | Audio file for the engine tests and `scripts/bench.sh` |

</details>

## Tests

```bash
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features
```

The interesting ones need real inputs and skip loudly without them:

| Test | What it proves |
| --- | --- |
| `transcribes_real_audio` | A file decodes and transcribes with no Python and no `ffmpeg` in the path |
| `exits_cleanly_with_a_model_loaded` | Quitting with a model resident does not abort |
| `idle_policy` | A model in active use is never collected; `0` disables collection |
| `reaper_frees_a_live_model_off_thread` | Freeing a live Metal context from a worker thread doesn't abort, the memory actually returns, and the engine still reloads. `#[ignore]`d — needs the weights |
| `benchmark_latency` | The numbers above. `#[ignore]`d — it is a measurement, not an assertion |
| `fresh_captures_survive` | The sweep never deletes a capture that may still belong to a dictation in flight |
| `other_files_are_left_alone` | Age alone is not licence to delete: only our own `.wav` captures are swept |
| `an_older_result_is_not_a_measurement` | A transcript from before timing existed records no timing, rather than a zero |

## Known gaps

Stated plainly, because a README that only lists wins is not worth reading:

- **Apple Silicon only.** The arm64 build does not run on Intel Macs, and Windows
  and Linux are not supported.
- **The AI half needs macOS 26 and Apple Intelligence.** Titles, overviews and
  ASK all run on Apple's on-device model, which does not exist before macOS 26
  and can be switched off or unavailable on an eligible Mac. Everything else —
  dictation, transcription, meeting recording, search, export — works on macOS
  11 and later regardless, and the app says which state it is in rather than
  failing quietly.
- **ASK answers from about six recordings at a time.** The on-device model has a
  4,096-token window shared between the question, the notes and the answer, so a
  question that really needs forty notes gets the six that matched best. It
  cites them, so you can see what it read.
- **Turning an answer into an email is unreliable.** Reformatting works —
  paragraph, bullets, shorter, a poem — but "make that an email" sometimes
  returns the input unchanged when the answer it is working from is thin.

## Roadmap

- Watch a folder for auto-transcription
- Local audio enhancement — podcast-grade cleanup, tunable
- Speaker names carried across meetings, not just within one
- One-click updates, once there is a signing key that never touches this repo

<div align="center">
<br>
<sub>Built by <a href="https://www.kupacreative.com">Kupa</a> · <a href="https://voicedumps.qwee.ai">voicedumps.qwee.ai</a></sub>
</div>
