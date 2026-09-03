/**
 * The shareable word cloud — still image and reel.
 *
 * Canvas rather than a screenshot of the panel: the thing being posted is
 * 1080×1920, and the panel is roughly 400px wide on screen. Scaling that up
 * gives soft type; drawing it means the export is sharp and its layout can be
 * composed for a phone screen instead of inheriting one made for a sidebar.
 *
 * 1080×1920 is 9:16 — Instagram Stories and Reels. Feed posts cap at 4:5
 * (1080×1350), so a Story is the surface this fits.
 *
 * One `paint` function serves both exports, taking a `Reveal` that says how far
 * along each part of the card is. The PNG paints it fully revealed; the reel
 * animates the same numbers. A second drawing routine for the video would have
 * drifted from this one within a week.
 *
 * Nothing here touches the network. The fonts are the two already bundled in
 * the app, and the pixels never leave the machine unless the user saves them.
 */

export type CloudWord = { word: string; count: number };

export const W = 1080;
export const H = 1920;
const MARGIN = 96;

/** Paper, ink and sage, matching `styles.css` — the app's own light surface. */
const PAPER = "#faf9f4";
const INK = "#1b2015";
const MUTED = "#8b8f83";
const FAINT = "#b3b6ab";
const SAGE = "#6d9455";
const SAGE_SOFT = "#b8d4a4";

const SANS = "'Space Grotesk', ui-sans-serif, system-ui, sans-serif";
const MONO = "'JetBrains Mono', ui-monospace, monospace";

/**
 * Type scale, largest first.
 *
 * Assigned by rank rather than by raw count. Counts in a young history are all
 * within a word or two of each other, so scaling linearly by frequency draws
 * twenty words at nearly the same size — a grey block. Rank guarantees a
 * hierarchy on day one and still reflects the ordering.
 */
const SIZES = [172, 148, 124, 124, 100, 100, 96, 82, 82, 78, 68, 68, 64, 64, 58, 58, 54, 54, 48,
  48, 46, 44, 42, 40, 38];

/**
 * Colour by rank, not by size.
 *
 * Keying off the font size put the ink/grey step wherever a line happened to
 * break, so a word could sit dark beside a lighter one of nearly the same size.
 * Rank moves in step with the type scale and the ramp stays monotonic.
 */
function ink(rank: number): string {
  if (rank < 2) return SAGE;
  if (rank < 10) return INK;
  if (rank < 18) return "#5f6459";
  return MUTED;
}

export type Placed = { text: string; x: number; y: number; size: number; rank: number };

/**
 * The site's entrance curve — `cubic-bezier(0.16, 1, 0.3, 1)`, the same one
 * `video/src/lib/anim.ts` calls `EXPO_OUT`.
 *
 * Solved directly rather than through GSAP so `paint` stays a pure function of
 * its `Reveal` and the still image can use it without a timeline. Newton on the
 * bezier's x, which converges in three iterations over this range.
 */
export function ease(t: number): number {
  if (t <= 0) return 0;
  if (t >= 1) return 1;
  const cx = 3 * 0.16;
  const bx = 3 * (0.3 - 0.16) - cx;
  const ax = 1 - cx - bx;
  const cy = 3 * 1;
  const by = 3 * (1 - 1) - cy;
  const ay = 1 - cy - by;
  let x = t;
  for (let i = 0; i < 5; i++) {
    const err = ((ax * x + bx) * x + cx) * x - t;
    const d = (3 * ax * x + 2 * bx) * x + cx;
    if (Math.abs(d) < 1e-6) break;
    x -= err / d;
  }
  return ((ay * x + by) * x + cy) * x;
}

/** Greedy line packing, centred, with the block centred vertically. */
export function layout(
  ctx: CanvasRenderingContext2D,
  words: CloudWord[],
  top: number,
  bottom: number,
): Placed[] {
  const maxWidth = W - MARGIN * 2;
  const items = words.slice(0, SIZES.length).map((w, i) => ({
    text: w.word,
    size: SIZES[i] ?? 38,
    rank: i,
  }));

  const lines: (typeof items)[] = [];
  let line: typeof items = [];
  let lineWidth = 0;
  const GAP = 30;

  for (const it of items) {
    ctx.font = `600 ${it.size}px ${SANS}`;
    const w = ctx.measureText(it.text).width;
    const next = lineWidth === 0 ? w : lineWidth + GAP + w;
    if (next > maxWidth && line.length) {
      lines.push(line);
      line = [it];
      lineWidth = w;
    } else {
      line.push(it);
      lineWidth = next;
    }
  }
  if (line.length) lines.push(line);

  // Line height follows the tallest word on the line, so a row of small words
  // doesn't leave the gap a row of large ones needs.
  const heights = lines.map((l) => Math.max(...l.map((i) => i.size)) * 1.18);
  const total = heights.reduce((a, b) => a + b, 0);
  let y = top + (bottom - top - total) / 2;

  const placed: Placed[] = [];
  lines.forEach((l, li) => {
    const h = heights[li];
    let width = 0;
    l.forEach((it, i) => {
      ctx.font = `600 ${it.size}px ${SANS}`;
      width += ctx.measureText(it.text).width + (i ? GAP : 0);
    });
    let x = (W - width) / 2;
    for (const it of l) {
      ctx.font = `600 ${it.size}px ${SANS}`;
      placed.push({ text: it.text, x, y: y + h * 0.74, size: it.size, rank: it.rank });
      x += ctx.measureText(it.text).width + GAP;
    }
    y += h;
  });
  return placed;
}

/**
 * A word reduced to the thing two spellings of it have in common.
 *
 * **Mirrors `normalise` in `src-tauri/src/brief.rs`, and has to.** That one
 * decides whether the model's sentence counts as being made of these words;
 * this one decides which words visibly move. A word the Rust side counts and
 * this side does not is a sentence that claims to be a rearrangement and
 * animates like a replacement.
 */
export function normalise(word: string): string {
  return Array.from(word)
    .filter((c) => /[\p{L}\p{N}]/u.test(c))
    .join("")
    .toLowerCase();
}

/** Where one word of the sentence ends up, and which cloud word it came from. */
export type Slot = {
  text: string;
  x: number;
  y: number;
  size: number;
  /**
   * Index into the `Placed` array this word travels from, or `null` for a word
   * the model supplied itself.
   */
  from: number | null;
};

/**
 * Sizes to try for the sentence, largest first.
 *
 * The sentence is at most twelve words and the card is 888px of usable width,
 * so which of these fits is decided by how long the words are rather than by
 * how many — "onboarding" and "waveform" cost four times what "the" does.
 * Trying rather than calculating because the answer depends on the font's own
 * metrics, which is what `measureText` is for.
 */
const SENTENCE_SIZES = [132, 118, 106, 96, 86, 76, 68, 60, 52, 44];

/**
 * How many lines the sentence may take.
 *
 * The band it sits in is the one the cloud filled, which is tall enough for far
 * more lines than this at any of the sizes above — so the cap is not about
 * fitting, it is about what the words look like when they get there. Past five
 * lines a sentence stops reading as one thing arriving and starts reading as a
 * paragraph, and the fitting loop is much better off dropping a size than
 * adding a sixth line.
 */
const MAX_SENTENCE_LINES = 5;

/**
 * A word the model brought is drawn smaller than a word the user said.
 *
 * Not decoration. The sentence has two kinds of word in it and the difference
 * matters: the big ones are what somebody actually spends their days saying,
 * and the small ones are grammar. Drawing them the same size would claim the
 * model wrote the sentence; drawing them apart shows what it did, which was
 * join six words that were already there.
 */
const JOIN_SCALE = 0.68;

/**
 * Lay the sentence out, and work out which cloud word each of its words is.
 *
 * Pure, and called every frame from `paint` for the same reason `layout` is:
 * `paint` is a function of its `Reveal` and nothing else, and a cache between
 * them is a thing that can be wrong.
 */
export function arrange(
  ctx: CanvasRenderingContext2D,
  sentence: string,
  placed: Placed[],
  top: number,
  bottom: number,
): Slot[] {
  const tokens = sentence.split(/\s+/).filter(Boolean);
  if (!tokens.length) return [];

  // Which cloud word each token came from — first come, first served, so a
  // sentence that says "meeting" twice moves two cloud words if the cloud has
  // two and one if it has one. Matching greedily rather than optimally because
  // the words are already in rank order: the first match is the biggest one,
  // which is the one worth moving.
  const taken = new Set<number>();
  const origin = tokens.map((t) => {
    const key = normalise(t);
    if (!key) return null;
    const i = placed.findIndex((p, j) => !taken.has(j) && normalise(p.text) === key);
    if (i < 0) return null;
    taken.add(i);
    return i;
  });

  const maxWidth = W - MARGIN * 2;

  for (const size of SENTENCE_SIZES) {
    const sizes = origin.map((o) => (o === null ? size * JOIN_SCALE : size));
    const gap = size * 0.3;

    const widths = tokens.map((t, i) => {
      ctx.font = `600 ${sizes[i]}px ${SANS}`;
      return ctx.measureText(t).width;
    });
    if (widths.some((w) => w > maxWidth)) continue;

    // Balanced, not greedy — the one place this differs from `layout`.
    //
    // Greedy packing fills each line to the margin and leaves whatever is left
    // on the last one, which is right for a cloud, where the ragged edge reads
    // as a shape. A sentence is read as language, and greedy gave
    // "Sarah fixed the / waveform / latency before the / pricing meeting" —
    // four lines of wildly different length, which is hard to read and looks
    // like a mistake. So the number of lines is worked out first, and the words
    // are spread evenly across that many.
    const run = widths.reduce((a, b) => a + b, 0) + gap * (tokens.length - 1);
    const count = Math.max(1, Math.ceil(run / maxWidth));

    const lines: number[][] = [];
    let line: number[] = [];
    let width = 0;
    // What is left to place, and how many lines are left to place it on. Both
    // shrink as lines are closed, so the target is recomputed rather than
    // fixed — a fixed share of the whole let each line overshoot by a word and
    // left the last one holding "meeting" on its own. Sharing out what
    // actually remains is self-correcting: a line that runs long makes the
    // next one's target shorter.
    let left = run;
    let lines_left = count;
    tokens.forEach((_, i) => {
      const next = line.length ? width + gap + widths[i] : widths[i];
      const target = left / Math.max(1, lines_left);
      // Break when the line is closer to its share without this word than with
      // it — the standard way to avoid breaking early on a word that merely
      // straddles the target. Never past the margin, and never onto a line
      // that was not planned for.
      const balanced =
        line.length > 0 &&
        lines_left > 1 &&
        next > target &&
        next - target > target - width;
      if (line.length && (next > maxWidth || balanced)) {
        lines.push(line);
        left -= width + gap;
        lines_left -= 1;
        line = [i];
        width = widths[i];
      } else {
        line.push(i);
        width = next;
      }
    });
    if (line.length) lines.push(line);
    // A size that needs more lines than this is the wrong size — try the next
    // one down rather than writing a paragraph.
    if (lines.length > MAX_SENTENCE_LINES) continue;

    const heights = lines.map((l) => Math.max(...l.map((i) => sizes[i])) * 1.26);
    const total = heights.reduce((a, b) => a + b, 0);
    // Try a smaller size rather than let the sentence run into the footer.
    if (total > bottom - top) continue;

    const slots: Slot[] = [];
    let y = top + (bottom - top - total) / 2;
    lines.forEach((l, li) => {
      const h = heights[li];
      const run = l.reduce((a, i, n) => a + widths[i] + (n ? gap : 0), 0);
      let x = (W - run) / 2;
      for (const i of l) {
        // Baselines share the line, so a small joining word sits on the same
        // line as the large words around it instead of floating in the middle
        // of them.
        slots.push({ text: tokens[i], x, y: y + h * 0.76, size: sizes[i], from: origin[i] });
        x += widths[i] + gap;
      }
      y += h;
    });
    return slots;
  }

  // Nothing fitted. Returning none means `paint` leaves the cloud alone, which
  // is the right failure: a sentence drawn over the footer would be worse than
  // no sentence at all.
  return [];
}

/**
 * The turn, as four beats of `Reveal.sentence`.
 *
 * | beat   | first starts | last ends |
 * | ------ | ------------ | --------- |
 * | leave  | 0.00         | 0.40      |
 * | gather | 0.08         | 0.70      |
 * | join   | 0.31         | 0.71      |
 * | settle | 0.77         | 1.00      |
 *
 * **Leave and gather overlap, and that is the whole character of it.** The
 * extras are still thinning out when the first survivor sets off, so the card
 * is never doing nothing — it clears and collects itself at the same time,
 * which is what makes the turn feel like one movement rather than a slideshow
 * of three states.
 *
 * This was how the first version worked, and it was the best of them. Two
 * rounds after it pulled the beats apart into a strict sequence, on the theory
 * that overlap was what made the middle illegible. It was not; the middle was
 * illegible because a word changed size and colour while it flew, so it could
 * not be followed. That is fixed where it belongs, in the drawing, and the
 * overlap is back.
 *
 * The one gap that remains is deliberate and is the last one: nothing at all
 * happens between 0.75 and 0.80, so the sentence is visibly finished and still
 * before it breathes.
 *
 * **Where the journey landed.** At a 3.4s phase a word takes 1.40s to travel
 * and the last sets off 0.71s after the first, against the first version's
 * 1.35s and 0.73s — within a frame or two of it on both counts. It got there
 * by being lengthened to 1.70s, watched, and then taken back up twice, which
 * is worth leaving on the record: the pace was right at the start and the two
 * rounds of slowing it down were the wrong fix for a real problem. What did
 * need fixing was the curve and the overlap, and those are what changed.
 *
 * Every number here is a fraction of the phase and multiplied by it, so
 * changing the phase length rescales the whole turn rather than silently
 * making the movement snappier — which is how two earlier rounds went wrong.
 */
const BEAT = {
  /**
   * The extras go, smallest first, so the cloud thins from its edges and
   * keeps its shape longest — losing the headline first reads as the card
   * breaking.
   */
  leaveLast: 0.14,
  leaveOver: 0.26,
  /**
   * The survivors gather, one after another, left to right, beginning while
   * the extras are still on their way out.
   *
   * `gatherOver` is a word's whole journey and is the number to change if the
   * rearrangement is hard to follow; `gatherLast` is the spread between the
   * first setting off and the last, and is the cheapest thing to overspend on,
   * since it lengthens the beat without making any single word clearer.
   */
  gatherFrom: 0.08,
  gatherLast: 0.21,
  gatherOver: 0.413,
  /** The grammar fades in under words that are still arriving. */
  joinFrom: 0.31,
  joinLast: 0.14,
  joinOver: 0.26,
  /**
   * And then — with the sentence finished and still for a beat first — it
   * takes a breath.
   *
   * The gap matters. This used to begin at the same instant the last joining
   * word stopped fading in, so the zoom read as part of the assembly rather
   * than as a response to it.
   */
  settleFrom: 0.77,
  /** Very slight on purpose: this is a sentence settling, not a title card. */
  settleScale: 0.04,
};

/**
 * The settle's own curve, and the reason it is not [`ease`].
 *
 * `ease` is the site's entrance curve — an expo-out that covers most of its
 * distance in the first fifth of the window. That is exactly right for
 * something arriving, which should land hard and settle, and exactly wrong for
 * a 4% scale: front-loaded, the whole zoom happens in a couple of frames and
 * reads as a jump rather than a breath.
 *
 * Smoothstep instead. It leaves and arrives at zero velocity, so the sentence
 * eases into the movement and eases out of it, and nothing about the moment
 * has an edge on it.
 */
function glide(t: number): number {
  return t * t * (3 - 2 * t);
}

/**
 * The travelling word's curve: smootherstep, one order gentler than [`glide`].
 *
 * Smoothstep leaves and arrives at zero velocity, which is already far better
 * than the expo-out this replaced. Smootherstep also leaves and arrives at zero
 * *acceleration*, so there is no kick at either end — the word does not so much
 * start as find itself moving.
 *
 * It is not a slower curve, which is the point. Its peak speed is higher than
 * smoothstep's (1.875 against 1.5, over the same distance and time), and the
 * journey it is used for was lengthened by the same quarter — so the fastest
 * moment of the movement is unchanged and all of the extra time is spent
 * easing in and out of it.
 */
function ease5(t: number): number {
  return t * t * t * (t * (t * 6 - 15) + 10);
}

/** `t` mapped onto the window `a`→`b`, clamped, before easing. */
function span(t: number, a: number, b: number): number {
  return Math.max(0, Math.min(1, (t - a) / (b - a)));
}

function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

/**
 * Two hex colours, mixed.
 *
 * The travelling words used to *switch* colour at the half-way point of their
 * journey — a hard change of ink on a word in mid-air, which is exactly the
 * moment the eye is trying to follow it. One of the two or three things that
 * made the rearrangement read as a dissolve instead of a move.
 */
function mix(from: string, to: string, t: number): string {
  const read = (hex: string) => [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
  const [ar, ag, ab] = read(from);
  const [br, bg, bb] = read(to);
  const at = (a: number, b: number) => Math.round(lerp(a, b, t));
  return `rgb(${at(ar, br)}, ${at(ag, bg)}, ${at(ab, bb)})`;
}

function label(
  ctx: CanvasRenderingContext2D,
  text: string,
  x: number,
  y: number,
  align: CanvasTextAlign = "left",
  colour = MUTED,
) {
  ctx.font = `500 22px ${MONO}`;
  ctx.fillStyle = colour;
  ctx.textAlign = align;
  // Every mono label in the app is letterspaced. Canvas only grew the property
  // recently, so it's set through a widened reference and simply has no effect
  // where WebKit doesn't support it — the label still draws, just tighter.
  const spaced = ctx as CanvasRenderingContext2D & { letterSpacing?: string };
  spaced.letterSpacing = "3px";
  ctx.fillText(text, x, y);
  spaced.letterSpacing = "0px";
  ctx.textAlign = "left";
}

/**
 * The app mark: `CLUSTERS.brand` from `PixelCluster`, drawn in canvas.
 *
 * The same nine cells with the same two knocked out, so the export carries the
 * actual logo rather than the generic square that stood in for it. `lit` fades
 * the cells in one at a time for the reel; the still image passes 1.
 */
function mark(ctx: CanvasRenderingContext2D, x: number, y: number, cell: number, lit = 1) {
  const BRAND = [true, true, false, true, true, true, false, true, true];
  const gap = cell * 0.5;
  BRAND.forEach((on, i) => {
    if (!on) return;
    // Cells light in reading order, each over a fifth of the reveal.
    const start = (i / BRAND.length) * 0.6;
    const a = Math.max(0, Math.min(1, (lit - start) / 0.4));
    if (a <= 0) return;
    ctx.globalAlpha = a;
    ctx.fillStyle = i === 4 ? SAGE : SAGE_SOFT;
    ctx.fillRect(
      x + (i % 3) * (cell + gap),
      y + Math.floor(i / 3) * (cell + gap),
      cell,
      cell,
    );
  });
  ctx.globalAlpha = 1;
}

/**
 * The paper texture, matching the app's own dotted canvas.
 *
 * A flat fill at this size reads as a blank page; the grid gives the card the
 * same printed-stock character the app has, and survives Instagram's
 * re-compression because the dots are a whole pixel.
 */
function grain(ctx: CanvasRenderingContext2D, alpha: number) {
  if (alpha <= 0) return;
  ctx.globalAlpha = alpha * 0.5;
  ctx.fillStyle = "#d8d6cc";
  for (let y = MARGIN; y < H - MARGIN; y += 32) {
    for (let x = MARGIN; x < W - MARGIN; x += 32) {
      ctx.fillRect(x, y, 2, 2);
    }
  }
  ctx.globalAlpha = 1;
}

export type CardData = {
  words: CloudWord[];
  notes: number;
  totalWords: number;
  period: string;
  /**
   * One sentence made out of these words by the on-device model.
   *
   * Optional, and the card is finished without it: there is no Apple
   * Intelligence on every Mac, and a model that writes something unusable is a
   * normal outcome rather than an error. See `arrange` for what having one
   * changes.
   */
  sentence?: string;
};

/**
 * How much of the card is showing.
 *
 * `words` is a count rather than a fraction so the reel can bring them in one
 * at a time; the fractional part is the word currently arriving.
 */
export type Reveal = {
  grain: number;
  header: number;
  words: number;
  footer: number;
  brand: number;
  /**
   * How far the cloud has rearranged itself into `CardData.sentence`, 0 to 1.
   *
   * A fraction rather than a count, unlike `words`: the words do not arrive one
   * at a time here, they all move at once with a stagger inside this one
   * number.
   */
  sentence: number;
};

/**
 * The card with everything on it — and the cloud still a cloud.
 *
 * `sentence: 0` is a decision, not an omission. The reel ends on the sentence,
 * and the still image does not, which breaks the rule that the last frame of
 * the video is the PNG. It is broken deliberately: the two exports answer
 * different questions. "What I talk about" is a picture of a month, and the
 * sentence is a punchline — worth watching arrive, and worth nothing as a
 * still, where it is just a line of text with a lot of paper around it.
 */
export const FULL: Reveal = {
  grain: 1,
  header: 1,
  words: Infinity,
  footer: 1,
  brand: 1,
  sentence: 0,
};

/** Draw the whole card at a given state of reveal. */
export function paint(ctx: CanvasRenderingContext2D, data: CardData, at: Reveal) {
  ctx.fillStyle = PAPER;
  ctx.fillRect(0, 0, W, H);
  ctx.textBaseline = "alphabetic";

  grain(ctx, at.grain);

  // -- header --------------------------------------------------------------
  if (at.header > 0) {
    ctx.globalAlpha = at.header;
    label(ctx, "WHAT I TALKED ABOUT", MARGIN, 150);
    label(ctx, data.period.toUpperCase(), W - MARGIN, 150, "right");
    ctx.fillStyle = FAINT;
    // The rule draws itself in from the left as the header lands.
    ctx.fillRect(MARGIN, 186, (W - MARGIN * 2) * at.header, 1);
    ctx.globalAlpha = 1;
  }

  // -- the words -----------------------------------------------------------
  const TOP = 268;
  const BOTTOM = H - 330;
  const placed = layout(ctx, data.words, TOP, BOTTOM);

  // Where each word is going, if it is going anywhere. Empty whenever there is
  // no sentence, the sentence would not fit, or the reel has not reached that
  // phase — and every branch below reads as the original code when it is.
  const slots =
    at.sentence > 0 && data.sentence
      ? arrange(ctx, data.sentence, placed, TOP, BOTTOM)
      : [];
  // Cloud word index → the slot it becomes.
  const going = new Map<number, { slot: Slot; nth: number; lead: boolean }>();
  // Which of the travelling words carry the sage.
  //
  // Not `rank < 2`, which is what the cloud uses. The sentence is written from
  // the words, not from the top of the list, and it is perfectly ordinary for
  // it to use neither of the two biggest — the first real sentence this was
  // tested on dropped both, and the card ended in flat ink with no accent left
  // anywhere on it. So the accent is re-earned: the two highest-ranked words
  // that actually make it into the sentence carry it, whatever their rank was
  // on the card. A word with `rank < 2` that travels is always one of them,
  // so nothing loses its sage by moving.
  const lead = new Set(
    slots
      .filter((s) => s.from !== null)
      .map((s) => s.from as number)
      .sort((a, b) => placed[a].rank - placed[b].rank)
      .slice(0, 2),
  );
  slots.forEach((slot, nth) => {
    if (slot.from !== null) {
      going.set(slot.from, { slot, nth, lead: lead.has(slot.from) });
    }
  });

  // The last beat: the whole finished sentence scales up a touch, about its own
  // centre. `arrange` centres the block horizontally on the card and vertically
  // in the band, so that centre is known without measuring anything.
  const settle =
    1 + BEAT.settleScale * glide(span(at.sentence, BEAT.settleFrom, 1));
  const midX = W / 2;
  const midY = (TOP + BOTTOM) / 2;

  placed.forEach((p, i) => {
    // Each word gets a whole unit of the counter to arrive in, and they
    // overlap: at `words = 4.3`, word 4 is 30% in while 3 is still settling.
    // Overlapping is what makes it read as a phrase assembling rather than a
    // metronome placing one word at a time.
    const raw = Math.max(0, Math.min(1, (at.words - i) / 1.6));
    if (raw <= 0) return;
    const a = ease(raw);

    const move = going.get(i);

    // Beat one. A word nobody needs for the sentence, on its way out.
    //
    // Smallest first, so the cloud thins from its edges and keeps its shape
    // longest — losing the headline first would read as the card breaking.
    // This is `1 - rank` because rank 0 is the *biggest* word: an earlier
    // version wrote `rank * 0.18` under a comment claiming small words went
    // first, and did exactly the opposite for weeks.
    if (slots.length && !move) {
      const rank = p.rank / Math.max(1, placed.length - 1);
      const from = (1 - rank) * BEAT.leaveLast;
      const gone = ease(span(at.sentence, from, from + BEAT.leaveOver));
      if (gone >= 1) return;
      ctx.save();
      ctx.font = `600 ${p.size}px ${SANS}`;
      const w = ctx.measureText(p.text).width;
      const cx = p.x + w / 2;
      // Shrinking about its own centre and sinking a little: a word that
      // simply faded on the spot left a hole where it had been, and twenty
      // holes appearing at once is what made this beat feel like a glitch.
      const shrink = 1 - 0.18 * gone;
      ctx.translate(cx, p.y);
      ctx.scale(shrink, shrink);
      ctx.translate(-cx, -p.y);
      ctx.translate(0, gone * p.size * 0.14);
      ctx.globalAlpha = a * (1 - gone);
      ctx.fillStyle = ink(p.rank);
      ctx.fillText(p.text, p.x, p.y);
      // A headline word that is not in the sentence takes its rule with it.
      // Leaving the rule to vanish on its own frame was the one thing in the
      // transition that read as a glitch rather than a departure.
      if (p.rank < 2) {
        ctx.globalAlpha = a * (1 - gone) * 0.5;
        ctx.fillStyle = SAGE_SOFT;
        ctx.fillRect(p.x, p.y + p.size * 0.16, w, Math.max(3, p.size * 0.045));
      }
      ctx.restore();
      return;
    }

    ctx.save();
    // The font has to be set before measuring, not after: `measureText` reads
    // whatever `ctx.font` currently is, so measuring first silently returned
    // the *previous* word's metrics — every underline came out the length of
    // the word before it.
    ctx.font = `600 ${p.size}px ${SANS}`;
    // Scale about the word's own centre so it grows into place instead of
    // sliding out from its left edge.
    const w = ctx.measureText(p.text).width;
    const cx = p.x + w / 2;
    ctx.translate(cx, p.y);
    ctx.scale(0.86 + 0.14 * a, 0.86 + 0.14 * a);
    ctx.translate(-cx, -p.y);
    // A short rise, scaled to the type: a 172px word travelling the same
    // distance as a 38px one looks like two different animations.
    ctx.translate(0, (1 - a) * p.size * 0.22);
    ctx.globalAlpha = a;

    ctx.fillStyle = ink(p.rank);

    if (move) {
      // Beat two. A word on its way into the sentence, setting off while the
      // extras are still leaving — see `BEAT`.
      //
      // Staggered by where it lands, not by how big it is, so the sentence
      // assembles left to right and can be read as it forms. The journeys
      // still overlap each other heavily; a strict queue over eight words
      // takes far too long to watch.
      const of = slots.length > 1 ? move.nth / (slots.length - 1) : 0;
      const from = BEAT.gatherFrom + of * BEAT.gatherLast;
      const raw = span(at.sentence, from, from + BEAT.gatherOver);
      // `ease5`, not the shared `ease`. That one is an expo-out: a word left
      // its place at full speed and crawled the last third, which is the
      // motion of something being *revealed*, not of something being moved.
      // Smootherstep starts and ends at rest and without a kick, so a word
      // looks picked up, carried and put down.
      const t = ease5(raw);
      // Size and colour lag the journey and only resolve over its last part.
      //
      // This is what makes the beat legible. A word that shrinks and changes
      // ink the whole way across cannot be followed — by the time it lands it
      // is not recognisably the thing that set off, so the eye reads a
      // dissolve. Holding its cloud appearance until it is nearly home means
      // you can track it the whole way, and the change happens where it
      // belongs: as it takes its place in the sentence.
      const settled = glide(span(raw, 0.55, 1));
      const { slot } = move;

      // Beat four rides on top of the journey, about the sentence's centre
      // rather than the word's, so the line grows as one thing.
      ctx.translate(midX, midY);
      ctx.scale(settle, settle);
      ctx.translate(-midX, -midY);

      // The sentence's spelling from the first frame, not the cloud's. The
      // cloud says "pricing" and the sentence says "Pricing," — swapping at
      // the end would mean a capital letter appearing on a word that has just
      // stopped moving, which is the one moment anybody is looking at it.
      const size = lerp(p.size, slot.size, settled);
      ctx.font = `600 ${size}px ${SANS}`;
      const now = ctx.measureText(slot.text).width;

      // Centres rather than left edges: the two ends are different sizes, so
      // interpolating the corner makes a shrinking word slide left as well as
      // travel, which reads as a wobble.
      ctx.font = `600 ${p.size}px ${SANS}`;
      const fromCx = p.x + ctx.measureText(slot.text).width / 2;
      ctx.font = `600 ${slot.size}px ${SANS}`;
      const toCx = slot.x + ctx.measureText(slot.text).width / 2;

      const x = lerp(fromCx, toCx, t) - now / 2;
      const y = lerp(p.y, slot.y, t);

      ctx.font = `600 ${size}px ${SANS}`;
      // Everything that is not carrying the accent settles to ink. A sentence
      // whose later words stay grey — which is what the cloud's own ramp would
      // give them — reads as half-finished rather than as a hierarchy.
      ctx.fillStyle = mix(ink(p.rank), move.lead ? SAGE : INK, settled);
      ctx.fillText(slot.text, x, y);

      // The sage rule follows its word all the way in.
      if (move.lead) {
        ctx.globalAlpha = a * 0.5;
        ctx.fillStyle = SAGE_SOFT;
        ctx.fillRect(x, y + size * 0.16, now, Math.max(3, size * 0.045));
      }
      ctx.restore();
      return;
    }

    ctx.fillText(p.text, p.x, p.y);

    // The two headline words are underlined as they land, the rule drawing out
    // from under the word with the same curve.
    if (p.rank < 2) {
      ctx.globalAlpha = a * 0.5;
      ctx.fillStyle = SAGE_SOFT;
      ctx.fillRect(p.x, p.y + p.size * 0.16, w * a, Math.max(3, p.size * 0.045));
    }
    ctx.restore();
  });

  // Beat three. The grammar, arriving quietly. It has nowhere to travel from —
  // these words were never on the card — so it fades in under words that are
  // still moving, and is settled just after the last of them lands.
  slots.forEach((slot, nth) => {
    if (slot.from !== null) return;
    const of = slots.length > 1 ? nth / (slots.length - 1) : 0;
    const from = BEAT.joinFrom + of * BEAT.joinLast;
    const a = ease(span(at.sentence, from, from + BEAT.joinOver));
    if (a <= 0) return;
    ctx.save();
    ctx.translate(midX, midY);
    ctx.scale(settle, settle);
    ctx.translate(-midX, -midY);
    ctx.globalAlpha = a;
    ctx.font = `600 ${slot.size}px ${SANS}`;
    ctx.fillStyle = MUTED;
    ctx.translate(0, (1 - a) * slot.size * 0.18);
    ctx.fillText(slot.text, slot.x, slot.y);
    ctx.restore();
  });

  // -- footer --------------------------------------------------------------
  if (at.footer > 0) {
    ctx.globalAlpha = at.footer;
    ctx.fillStyle = FAINT;
    ctx.fillRect(MARGIN, H - 250, (W - MARGIN * 2) * at.footer, 1);
    label(
      ctx,
      `${data.notes} NOTES · ${data.totalWords.toLocaleString()} WORDS · SPOKEN, NOT TYPED`,
      MARGIN,
      H - 200,
    );
    ctx.globalAlpha = 1;
  }

  // -- branding ------------------------------------------------------------
  if (at.brand > 0) {
    mark(ctx, MARGIN, H - 158, 14, at.brand);
    ctx.globalAlpha = Math.max(0, Math.min(1, (at.brand - 0.5) * 2));
    ctx.font = `600 27px ${SANS}`;
    ctx.fillStyle = INK;
    ctx.fillText("VoiceDumps", MARGIN + 76, H - 130);
    label(ctx, "VOICEDUMPS.QWEE.AI", W - MARGIN, H - 130, "right", FAINT);
    ctx.globalAlpha = 1;
  }
}

/** A canvas at export size, with the bundled fonts guaranteed loaded. */
export async function board(): Promise<{
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
}> {
  // `document.fonts.ready` alone is not enough, and the failure is silent.
  //
  // It resolves once everything *already requested* has settled — and a face
  // that no DOM node has used yet was never requested, so it resolves happily
  // without it. Canvas then substitutes a system sans, `measureText` returns
  // that face's widths, and the card is laid out to the wrong metrics: the
  // first export after a cold launch broke its lines in different places from
  // every one after it. Asking for each face by name is what actually loads it.
  await Promise.all([
    document.fonts.load(`600 172px ${SANS}`),
    document.fonts.load(`600 38px ${SANS}`),
    document.fonts.load(`500 22px ${MONO}`),
  ]);
  await document.fonts.ready;
  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("could not open a canvas");
  return { canvas, ctx };
}

/** Draw the card and hand back PNG bytes. */
export async function renderWordCloud(data: CardData): Promise<Uint8Array> {
  const { canvas, ctx } = await board();
  paint(ctx, data, FULL);

  const blob: Blob = await new Promise((resolve, reject) =>
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error("could not encode the image"))),
      "image/png",
    ),
  );
  return new Uint8Array(await blob.arrayBuffer());
}
