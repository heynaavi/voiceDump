import type { Word } from "./api";

/**
 * Re-fit a paragraph's word timings onto edited text.
 *
 * Naively marking the whole paragraph "edited" throws away timing for every
 * word the user didn't touch — fix one mis-transcribed name and follow-along
 * dies for the surrounding minute. Instead we align old words to new ones and
 * keep every timing that survived, so a typo fix costs exactly that word.
 *
 * Words with no counterpart in the original are marked `edited`, which is what
 * tints them in the transcript.
 */
export function reconcileWords(oldWords: Word[], newText: string): Word[] {
  const next = newText.split(/\s+/).filter(Boolean);
  if (!oldWords.length || !next.length) return [];

  const prev = oldWords.map((w) => normalise(w.text));
  const matches = align(
    prev,
    next.map(normalise),
  );

  const out: Word[] = [];
  // Runs of unmatched new words sit between two anchors; their timing is
  // interpolated across whatever gap the anchors leave.
  let pending: number[] = [];
  let lastEnd = oldWords[0].start;

  const flush = (until: number) => {
    if (!pending.length) return;
    const span = Math.max(0, until - lastEnd);
    const each = span / pending.length;
    pending.forEach((ni, k) => {
      out.push({
        start: lastEnd + each * k,
        end: lastEnd + each * (k + 1),
        text: next[ni],
        edited: true,
      });
    });
    pending = [];
  };

  next.forEach((token, ni) => {
    const oi = matches.get(ni);
    if (oi === undefined) {
      pending.push(ni);
      return;
    }
    const src = oldWords[oi];
    flush(src.start);
    out.push({ start: src.start, end: src.end, text: token, edited: false });
    lastEnd = src.end;
  });

  flush(oldWords[oldWords.length - 1].end);
  return out;
}

/** Trailing punctuation and case shouldn't count as an edit. */
function normalise(s: string): string {
  return s.toLowerCase().replace(/[^\p{L}\p{N}']/gu, "");
}

/**
 * Longest common subsequence, returning new-index → old-index for matched
 * tokens. Paragraphs are a few hundred words at most, so the O(n·m) table is
 * comfortably cheap and far simpler than a diff library.
 */
function align(prev: string[], next: string[]): Map<number, number> {
  const n = prev.length;
  const m = next.length;
  const table: number[][] = Array.from({ length: n + 1 }, () =>
    new Array(m + 1).fill(0),
  );

  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      table[i][j] =
        prev[i] === next[j]
          ? table[i + 1][j + 1] + 1
          : Math.max(table[i + 1][j], table[i][j + 1]);
    }
  }

  const matches = new Map<number, number>();
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (prev[i] === next[j]) {
      matches.set(j, i);
      i++;
      j++;
    } else if (table[i + 1][j] >= table[i][j + 1]) {
      i++;
    } else {
      j++;
    }
  }
  return matches;
}
