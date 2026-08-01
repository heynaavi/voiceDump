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
};

export const FULL: Reveal = { grain: 1, header: 1, words: Infinity, footer: 1, brand: 1 };

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
  const placed = layout(ctx, data.words, 268, H - 330);
  placed.forEach((p, i) => {
    const a = Math.max(0, Math.min(1, at.words - i));
    if (a <= 0) return;
    ctx.save();
    ctx.globalAlpha = a;
    // Each word rises a little as it arrives, so the block assembles rather
    // than blinking on. At a=1 the offset is zero and the still image is
    // identical to a fully-revealed frame.
    ctx.translate(0, (1 - a) * 26);
    ctx.font = `600 ${p.size}px ${SANS}`;
    ctx.fillStyle = ink(p.rank);
    ctx.fillText(p.text, p.x, p.y);
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
