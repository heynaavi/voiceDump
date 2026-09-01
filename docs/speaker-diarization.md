# Who said what

Whisper stays. This is an addition, and the distinction is not a technicality —
it decides the whole design.

## Why the model does not change

Diarization is not a feature an ASR model has or lacks. It is a separate
pipeline — segmentation, then speaker embeddings, then clustering — that answers
one question Whisper never attempts: *is this the same voice as that?* No version
of Whisper will ever answer it, and no better ASR model would remove the need for
a second one.

Which means the language question answers itself. **Diarization never looks at
words.** It clusters voice timbre, so Hindi, English and a sentence that switches
between them mid-clause are identical to it. Whisper keeps doing the words in all
99 languages it knows; the diarizer only ever says "same voice" or "different
voice". Nothing about language coverage is at risk here, which is exactly why
this is the change worth making and swapping the ASR is not.

For the record on swapping it: Parakeet TDT 0.6B v3 does beat whisper-large-v3 —
9.7% against 9.9% WER over 24 languages, 5.3% against 5.8% on common ones, at
around 680 MB int8 and considerably faster. It also covers 25 European languages
where Whisper covers 99. That single fact ends the argument for this app.

And Meta's Muse Voice Transcribe, which does streaming ASR and diarization and
endpointing together and is better than any of this: **API only.** No open
weights. It cannot go in a product whose first promise is that nothing leaves the
machine.

## What already exists here

Almost all of it, which is the pleasant surprise:

- `Paragraph.speaker` — a label, already free text, already renameable
- `Paragraph.side` — `"you"` or `"others"`, a fact about which microphone heard it
- `rename_speaker` / `relabel` — rewrites the turns *and* the raw segments, so a
  rename survives anything that re-derives paragraphs later
- **word-level timestamps**, shipped in 1.1.2

That last one is the join key and the reason this is cheap. Diarization emits
speaker turns as time ranges. Word timings place every word on the same
timeline. Assigning a speaker to a word is an interval intersection over data
already in the database — no re-transcription, no model change, no new schema.

## The decisions

**1. `side` is never written by the diarizer.** It is ground truth from a
microphone. `speaker` is, in the existing comment's words, "a name somebody chose
and can rename" — a guess about a voice is the same *kind* of thing, so it goes
there and nowhere else. The two-sided colouring in the reading view keeps meaning
what it means.

**2. The `you` track is never diarized.** On a meeting your own microphone is
definitionally you. Running a clusterer across it cannot discover a second person;
it can only invent one. `You` stays exact, and the diarizer runs on the `others`
track only.

**3. Labels are `Speaker 1`, `Speaker 2`, numbered by who spoke first.** Not
pyannote's `SPEAKER_00` — those ids are arbitrary and change between runs.
Ordinal by first speech is stable and means something: Speaker 1 opened the call.
Numbers rather than letters because they read correctly in prose and in the
exported Markdown (`Speaker 2: …`) and do not run out at twenty-six.

**4. A track with one voice in it is not relabelled.** If the `others` side turns
out to be one person, it stays `Others`. `Speaker 1` on a one-to-one call replaces
a word that means something with a number that means nothing. Only two or more
found speakers earn labels.

**5. A diarized label is a default, not an answer.** `rename_speaker` already
turns `Speaker 2` into `Priya` everywhere, including the segments. Nothing new is
needed for naming — the feature is that there is now something worth naming.

**6. Where it actually pays.** Two-track capture already answers *you vs them*.
Diarization answers *which of them* — and, more importantly, it is the only thing
that helps single-track recordings at all. A meeting recorded in a room on one
microphone is today a single undivided wall of text. That is the case with the
most to gain.

## Open, pending the prototype

- **When it runs.** At ingest for meetings, or on demand behind a button? Long
  notes already brief themselves in the background, so there is a pattern for
  doing it unasked — but only if it is fast enough to be invisible.
- **How honest to be visually.** A diarized name is a guess and a typed name is
  not. Whether the reading view should say so is a real question, not an obvious
  yes: a hedge on every line is its own kind of noise.
- **Identity across recordings.** Voice embeddings could recognise a named
  speaker in next week's meeting. That is a much larger feature and explicitly not
  in this pass.

## What it actually does, measured

`sherpa-onnx` v1.13.7, pyannote segmentation 3.0 int8 (1.5 MB) plus a speaker
embedder, both on CoreML. **6.3× realtime** — 751 s of audio in 119 s — on the
Neural Engine, so it does not contend with Whisper for the Metal GPU.

**The defaults are wrong, and not because of our audio.** On sherpa's own
four-speaker reference file, the shipped threshold of 0.5 reports *seven*
speakers. Calibrating against that known answer:

| threshold | TitaNet | 3D-Speaker |
| --- | --- | --- |
| 0.6 | 6 | 6 |
| **0.8** | **4 — correct** | 5 |
| 1.0 | 3 | 3 |

TitaNet (40 MB) hits it exactly; 3D-Speaker never does at any threshold. So
TitaNet is the embedder.

**On a real 14-minute call, 0.8 gives 11 speakers and it takes 1.2 to get the
right two:**

| threshold | speakers | attribution | mapping |
| --- | --- | --- | --- |
| 0.9 | 9 | 70.9% | 7→others, 2→you |
| 1.0 | 5 | 69.5% | 4→others, 1→you |
| 1.1 | 3 | 68.6% | 2→others, 1→you |
| 1.2 | **2** | 68.5% | 1→others, 1→you |

Baseline — labelling everything as whoever spoke most — is 51.4%. Accuracy
barely moves while clusters collapse from nine to two, which is the healthy
shape: the extra clusters were sub-splits of the same people rather than
confusions between them.

The obvious reading of that — that longer audio needs a larger threshold — was
wrong, and worth recording as wrong because it would have been an expensive
thing to design around.

## 0.8 is the threshold, at every length

The real recordings could not settle it: they differed in speaker, language,
room and duration all at once. So `scripts/make-diarization-fixture.py` builds
two files from the *same* four voices and the same kind of content, 1.6 minutes
and 28.7 minutes, making length the only variable.

| threshold | short — 1.6 min | long — 28.7 min |
| --- | --- | --- |
| 0.6 | 5 clusters, 4/4 speakers, 95.9% | 6 clusters, 4/4, 99.5% |
| **0.8** | **4 clusters, 4/4, 93.2%** | **4 clusters, 4/4, 99.2%** |
| 1.0 | 3 clusters, 3/4, 76.4% | 3 clusters, 3/4, 72.7% |
| 1.2 | 1 cluster, 30.1% | 2 clusters, 52.3% |

0.8 recovers exactly four speakers at both lengths, and the same value was
already correct on the 57-second Mandarin reference. Length is not the variable.
If anything the longer file is *easier* — 99.2% against 93.2% — because more
speech per person makes each cluster better defined.

So what made the real call need 1.2 is the recording, not its duration:
codec artefacts on the far side, level drift, and the two sides bleeding into
each other through speakers all widen a speaker's own spread until the clusterer
splits them. That is a property of the audio and cannot be fixed by a constant.

**Which points at the fix.** On real audio the extra clusters are sub-splits of
one person, not confusions between people — attribution moved only 70.9% → 68.5%
while clusters fell from nine to two. So over-clustering costs almost nothing in
correctness and everything in presentation: nobody wants to see eleven speakers
in a two-person call. Run at the validated 0.8 and merge down afterwards, or let
someone say how many people were in the room, rather than bending the threshold
per recording.

**And the ceiling is synthetic.** 93–99% is what clean TTS voices with no room,
no overlap and no crosstalk look like. Real conversation will be well below it;
the fixture's job was to isolate one variable, not to predict a shipping
number.

## The conclusion that changes the plan

**Diarization should not run on meetings at all.**

Two-track capture already knows who was speaking, because it recorded the two
sides separately. That is a fact, and 68.5% is a guess. Replacing an exact
answer with a good one is a downgrade no threshold fixes — and the entire
benchmark above is only a benchmark because the true answer was already sitting
in the database to grade against.

What diarization is *for* is the case with no second track: a conversation
recorded in a room on one microphone, an interview, a dropped file. Today those
are one undivided wall of text and anything above the 51.4% baseline is new
information. None of the recordings on this machine are of that kind, so **the
quality question is still open** — the numbers above measure the case we should
not use it for.

## Two things found on the way

**One meeting's audio is defective.** In `659c7b19a9518` the silence floor is RMS
1030 and the user's own voice measures 968 — quieter than the room tone — while
the far side reaches 3003. `Editing Techniques` on the same code is balanced:
3398 / 3369 with a silence floor of 7. Whisper never noticed because it
transcribes each side before the mixdown, but playback of that meeting is
missing half the conversation. Worth its own investigation.

**Benchmarks need the right ground truth twice over.** The first attempt scored
against paragraphs, which are turn-groupings spanning up to forty seconds
*including pauses*, so both sides overlapped almost everywhere and the metric
could not separate a good clusterer from a coin flip. The second scored against
the defective recording above. Both produced confident, meaningless numbers.

## How it ships

**The models download; the runtime cannot.** That split is not a preference, it
is what the code signature allows.

| | size | where it lives |
| --- | --- | --- |
| pyannote segmentation, int8 | 1.5 MB | downloaded on demand |
| TitaNet embeddings | 40 MB | downloaded on demand |
| `libonnxruntime.dylib` | 27 MB | **in the bundle** |
| `libsherpa-onnx-c-api.dylib` | 2.9 MB | **in the bundle** |

The two models are data and go straight into `models.rs`, which already has the
`Spec` table, SHA-256 verification, progress events and a resumable fetch. Two
more rows and the "download it the first time it is needed" behaviour is free —
nobody re-downloads anything, and the app does not carry 42 MB it may never use.

The libraries are a different thing, and the reason is worth writing down because
the obvious plan does not work. The app is signed `adhoc,runtime` — the hardened
runtime is on, and under it library validation refuses to load any dylib not
signed by the same identity. A downloaded `libonnxruntime.dylib` would be
rejected at load time, not at download time. Shipping it as a download would mean
adding `com.apple.security.cs.disable-library-validation`, which turns off code
integrity for the whole process — a poor trade in an app whose first promise is
that nothing leaves the machine, and a much worse one than 30 MB of disk.

So the DMG goes from 4.9 MB to roughly 33 MB. Worth keeping in proportion: a
first launch already downloads 539 MB of Whisper weights, so this is 6% of what a
new user fetches anyway, and it buys a feature that works offline forever after.

**Not the older runtime.** onnxruntime 1.17.1 is 23 MB rather than 27, and on the
same fixture at the same threshold it scores 80.0% where 1.28.0 scores 93.2%.
Four megabytes for thirteen points is the wrong direction.

## What is left to build

1. **`models.rs`** — two `Spec` rows, with the diarizer's models optional rather
   than `required()`, so an existing user is not made to fetch 42 MB for a
   feature they have not asked for.
2. **The helper.** Ship `sherpa-onnx-offline-speaker-diarization` and its two
   dylibs as bundle resources and spawn it, exactly as the Swift helpers are
   spawned today. It keeps ONNX out of the Rust binary and matches a pattern the
   codebase already has four instances of.
3. **A parser** for its `start -- end speaker_N` output into [`Turn`], next to
   the merge logic that is already written and tested.
4. **The gate.** Single-track sources only. Meetings already know who spoke and
   must not be overwritten — see above.
5. **Merging down.** At 0.8 a real recording over-clusters, and the surplus is
   sub-splits of one person rather than confusions between people. Merge nearby
   clusters, or take a count from the user, before anything is shown.
6. **A setting, defaulting off for beta.** The one case this feature exists for —
   a room, one microphone, several people — has never been measured, because no
   recording of that kind exists on this machine. Beta users are how that gets
   answered, and a switch is how they stop paying for it if the answer is bad.
