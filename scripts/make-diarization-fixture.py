#!/usr/bin/env python3
"""Build a four-speaker English conversation with exact ground truth.

Diarization cannot be graded on a recording whose true speaker labels are a
guess, and every recording on this machine either has two-track labels (a fact,
but only two speakers) or none at all. So this synthesises one: four distinct
macOS voices, each line rendered separately, the real durations measured, and the
speaker intervals written out beside the audio.

**These are synthetic voices and they are easier than people.** No room, no
overlap, no crosstalk, consistent level and pitch across the whole file. Numbers
from this fixture are an upper bound and must never be quoted as though they came
off a real conversation — its job is to answer questions the real recordings
cannot: does this work in English at all, and does the right threshold move with
the length of the audio?

That second question is the reason it builds two files from the same voices and
the same kind of content. A threshold that has to change between them is a
property of duration, not of the speakers — which is exactly what the real
recordings hinted at (0.8 for a 57-second clip, 1.2 for a fourteen-minute call)
and could not prove, because they differed in speaker, language and room as well.

    python3 scripts/make-diarization-fixture.py --out DIR
"""
import argparse, json, os, subprocess, wave

# Two male, two female, four accents — chosen to be separable by ear so that a
# clusterer failing on them is failing at something a person finds easy.
VOICES = [("Daniel", "British male"), ("Karen", "Australian female"),
          ("Aman", "Indian male"), ("Kathy", "American female")]

TURNS = [
 "Right, let's start with where the build actually stands this week.",
 "The transcription side is finished. I ran it against three long recordings yesterday and none of them dropped a word.",
 "That matches what I saw. The only thing I could not reproduce was the timing issue on very long files.",
 "I think that one was fixed when we changed how the audio buffer is allocated.",
 "Good. What about the export formats, are we still shipping all four?",
 "All four, yes. PDF, markdown, plain text, and the word level timings file.",
 "The word timings one is the interesting part. Nobody else gives you that locally.",
 "Agreed. It is the thing people will actually notice, once they try lining a transcript up against the audio.",
 "How long does a typical hour of recording take to process end to end?",
 "About four minutes on this machine, and it does not block anything else while it runs.",
 "That is faster than I expected, honestly.",
 "It got much faster once we stopped reloading the model between runs.",
 "Are there any open problems that would stop us releasing this month?",
 "One. The speaker labelling still needs a proper test on a real room recording.",
 "I can record something next week with a few people around a table.",
 "That would help a lot. Everything we have tested so far had the speakers on separate channels.",
 "Which makes it easy, and not representative.",
 "Exactly. The hard case is one microphone and three people interrupting each other.",
 "Let us plan for that then, and keep the release date as it is.",
 "Fine by me. I will write up what we covered and send it round this afternoon.",
]


def render(text, voice, path):
    aiff = path + ".aiff"
    subprocess.run(["say", "-v", voice, "-o", aiff, text], check=True)
    subprocess.run(["/usr/bin/afconvert", "-f", "WAVE", "-d", "LEI16@16000",
                    "-c", "1", aiff, path], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    os.remove(aiff)
    with wave.open(path) as w:
        return w.getnframes() / w.getframerate()


def build(dest, rounds, tmp):
    frames, truth, clock = [], [], 0.0
    for r in range(rounds):
        for i, text in enumerate(TURNS):
            voice = VOICES[i % len(VOICES)][0]
            # Vary the wording between rounds so a long file is not the same
            # audio repeated — identical clips would cluster perfectly and
            # flatter the result.
            line = text if r == 0 else f"{text} That was point {r + 1}."
            clip = os.path.join(tmp, f"c{r}-{i}.wav")
            seconds = render(line, voice, clip)
            with wave.open(clip) as w:
                frames.append(w.readframes(w.getnframes()))
            truth.append({"speaker": voice, "start": round(clock, 3),
                          "end": round(clock + seconds, 3)})
            clock += seconds
            os.remove(clip)

    out = wave.open(dest, "w")
    out.setnchannels(1); out.setsampwidth(2); out.setframerate(16000)
    for f in frames:
        out.writeframes(f)
    out.close()
    json.dump(truth, open(dest.replace(".wav", ".json"), "w"), indent=1)
    return clock


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--out", required=True)
    a = p.parse_args()
    os.makedirs(a.out, exist_ok=True)
    tmp = os.path.join(a.out, "tmp"); os.makedirs(tmp, exist_ok=True)

    for name, rounds in (("four-speakers-short.wav", 1), ("four-speakers-long.wav", 14)):
        dest = os.path.join(a.out, name)
        secs = build(dest, rounds, tmp)
        print(f"{name}: {secs/60:.1f} min, {len(VOICES)} speakers, "
              f"{rounds * len(TURNS)} turns")
    os.rmdir(tmp)
