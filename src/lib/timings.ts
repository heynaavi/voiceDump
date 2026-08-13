//! When each word was said.
//!
//! Whisper hands back a time for every token it emitted, and the app has always
//! kept them — that is what lights the word you are hearing while the audio
//! plays. This turns the same data into something another tool can read.
//!
//! The awkward part is that a whisper token is not a word, so both consumers
//! need the same reconstruction; `placeWords` lives here rather than in the
//! reading view so there is one answer to "where does this token sit in the
//! text" instead of two that drift.

import type { Paragraph } from "./api";
import { formatTimestamp } from "./format";

export type Placed = { prefix: string; text: string; from: number; to: number };

/**
 * Lay a paragraph's words back onto its own text.
 *
 * The two are not in the same shape, and neither one can be spaced by rule:
 *
 * - Fresh from whisper, `words` are *BPE tokens*, not words. "graphify" arrives
 *   as `" graph"` + `"ify"`, and each token carries the whitespace that belongs
 *   in front of it — punctuation like `","` carries none.
 * - After an edit, `reconcileWords` splits the typed text on whitespace, so its
 *   tokens are bare words with no spacing encoded at all.
 *
 * Joining with a space is right for the second and wrong for the first, which
 * is what put a gap in the middle of "graph ify" and in front of every comma.
 * Concatenating raw is right for the first and wrong for the second. There is no
 * per-token test that separates them.
 *
 * So neither is used as the source of spacing: `text` is. Each token is found in
 * the paragraph's own text in order, and whatever sits between one token and the
 * next is emitted verbatim. The rendered characters are then `text`, exactly —
 * which is what COPY has always used, and why that button was already correct
 * while selecting the same words with a cursor was not.
 *
 * The offsets come back too, so search matches land on the right words instead
 * of being hunted for in a string nothing renders.
 */
export function placeWords(
  text: string,
  words: { text: string }[],
): Placed[] {
  const out: Placed[] = [];
  let at = 0;
  for (const w of words) {
    const token = w.text.trim();
    // One entry per input word, always: `flatWords` and the highlight sets key
    // words by their index in `p.words`, and a skipped token here would shift
    // every following word onto the wrong timing.
    if (!token) {
      out.push({ prefix: "", text: "", from: at, to: at });
      continue;
    }
    const found = text.indexOf(token, at);
    if (found === -1) {
      // Timings that no longer describe this text — a reconcile that dropped
      // out of step. Keep the word clickable and readable rather than losing it.
      out.push({
        prefix: out.length ? " " : "",
        text: token,
        from: at,
        to: at + token.length,
      });
      continue;
    }
    out.push({
      prefix: text.slice(at, found),
      text: token,
      from: found,
      to: found + token.length,
    });
    at = found + token.length;
  }
  return out;
}

/** One whole word, as a reader would count it, and when it was said. */
export type TimedWord = {
  /** Seconds from the start of the recording. */
  start: number;
  end: number;
  text: string;
  /**
   * Typed by hand rather than heard. Its timing was interpolated across the gap
   * the surrounding words left, so it is a guess and is labelled as one.
   */
  edited: boolean;
};

/**
 * A paragraph's tokens, glued back into words.
 *
 * The glue rule falls straight out of `placeWords`: whatever sits between one
 * token and the next is the paragraph's own text, so a token with no whitespace
 * in front of it is a continuation of the word before it. That is what puts
 * `" graph"` + `"ify"` back together as one word and keeps `","` attached to
 * whatever it followed, without needing to know anything about the tokeniser.
 *
 * A word spanning several tokens takes the first token's start and the last
 * one's end, and counts as typed if any part of it was.
 */
export function timedWords(paragraph: Paragraph): TimedWord[] {
  const tokens = paragraph.words ?? [];
  const placed = placeWords(paragraph.text, tokens);
  const out: TimedWord[] = [];

  placed.forEach((token, i) => {
    // Whisper emits empty tokens; there is no word there to time.
    if (!token.text) return;
    const source = tokens[i];

    if (out.length && !/\s/.test(token.prefix)) {
      const last = out[out.length - 1];
      last.text += token.text;
      last.end = Math.max(last.end, source.end);
      last.edited = last.edited || !!source.edited;
      return;
    }

    out.push({
      start: source.start,
      end: source.end,
      text: token.text,
      edited: !!source.edited,
    });
  });

  return out;
}

/** Does this transcript carry per-word timing anywhere? */
export function hasWordTimings(paragraphs: Paragraph[]): boolean {
  return paragraphs.some((p) => p.words?.length);
}

/**
 * The transcript as Markdown with a time on every word.
 *
 * Written for two readers at once. A person gets headings they can scan, the
 * speaker on each one, and italics marking the words that were typed rather
 * than heard. A script gets one word per line in a fixed shape —
 * `` - `START END` WORD `` — which splits on the first two whitespace-separated
 * numbers inside the backticks and takes the rest of the line as the word.
 *
 * Seconds rather than clock stamps, because every tool that consumes timings
 * wants a number it can subtract; the human-readable stamp is in the heading
 * above, where subtracting it is nobody's problem.
 */
export function wordTimingsMarkdown(doc: {
  title: string;
  dateline: string;
  paragraphs: Paragraph[];
}): string {
  const lines: string[] = [
    `# ${doc.title}`,
    "",
    `*${doc.dateline}*`,
    "",
    "Word-level timings. Every word is one list item, and the two numbers in",
    "front of it are seconds from the start of the recording — when the word",
    "begins and when it ends. Words in *italics* were typed rather than heard,",
    "so their timing is interpolated from the words either side rather than",
    "measured.",
  ];

  for (const p of doc.paragraphs) {
    const heading = p.speaker
      ? `## ${formatTimestamp(p.start)} · ${p.speaker}`
      : `## ${formatTimestamp(p.start)}`;
    lines.push("", heading, "");

    const words = timedWords(p);
    if (!words.length) {
      // Old transcripts, and anything read before word timings were stored.
      // Say so and print the paragraph, rather than leaving a silent hole that
      // reads as a gap in the recording.
      lines.push("No per-word timings for this paragraph.", "", p.text);
      continue;
    }

    for (const w of words) {
      const text = w.edited ? `*${w.text}*` : w.text;
      lines.push(`- \`${w.start.toFixed(2)} ${w.end.toFixed(2)}\` ${text}`);
    }
  }

  return lines.join("\n") + "\n";
}
