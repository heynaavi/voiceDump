/**
 * The shareable word cloud, drawn at full resolution.
 *
 * Canvas rather than a screenshot of the panel: the thing being posted is
 * 1080×1920, and the panel is roughly 400px wide on screen. Scaling that up
 * gives soft type; drawing it means the export is sharp and its layout can be
 * composed for a phone screen instead of inheriting one made for a sidebar.
 *
 * 1080×1920 is 9:16 — Instagram Stories and Reels. Feed posts cap at 4:5
 * (1080×1350), so a Story is the surface this fits.
 *
 * Nothing here touches the network. The fonts are the two already bundled in
 * the app, and the pixels never leave the machine unless the user saves them.
 */

export type CloudWord = { word: string; count: number };

const W = 1080;
const H = 1920;
const MARGIN = 96;

/** Paper, ink and sage, matching `styles.css` — the app's own light surface. */
const PAPER = "#faf9f4";
const INK = "#1b2015";
const MUTED = "#8b8f83";
const FAINT = "#b3b6ab";
const SAGE = "#6d9455";

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

type Placed = { text: string; x: number; y: number; size: number; rank: number };

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

/** Greedy line packing, centred, with the block centred vertically. */
function layout(
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

function label(ctx: CanvasRenderingContext2D, text: string, x: number, y: number, align: CanvasTextAlign = "left") {
  ctx.font = `500 22px ${MONO}`;
  ctx.fillStyle = MUTED;
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

export type CardData = {
  words: CloudWord[];
  notes: number;
  totalWords: number;
  period: string;
};

/**
 * Draw the card and hand back PNG bytes.
 *
 * Waits on `document.fonts.ready` first: canvas silently substitutes a system
 * face for one that has not finished loading, and the whole card is type.
 */
export async function renderWordCloud(data: CardData): Promise<Uint8Array> {
  await document.fonts.ready;

  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("could not open a canvas");

  ctx.fillStyle = PAPER;
  ctx.fillRect(0, 0, W, H);

  ctx.textBaseline = "alphabetic";

  // -- header --------------------------------------------------------------
  label(ctx, "WHAT I TALKED ABOUT", MARGIN, 150);
  label(ctx, data.period.toUpperCase(), W - MARGIN, 150, "right");

  ctx.fillStyle = FAINT;
  ctx.fillRect(MARGIN, 186, W - MARGIN * 2, 1);

  // -- the words -----------------------------------------------------------
  for (const p of layout(ctx, data.words, 260, H - 320)) {
    ctx.font = `600 ${p.size}px ${SANS}`;
    ctx.fillStyle = ink(p.rank);
    ctx.fillText(p.text, p.x, p.y);
  }

  // -- footer --------------------------------------------------------------
  ctx.fillStyle = FAINT;
  ctx.fillRect(MARGIN, H - 250, W - MARGIN * 2, 1);

  label(
    ctx,
    `${data.notes} NOTES · ${data.totalWords.toLocaleString()} WORDS · SPOKEN, NOT TYPED`,
    MARGIN,
    H - 200,
  );

  // Branding: a sage square and the name. Deliberately the same weight as the
  // rest of the footer — a share card that shouts its own logo gets cropped.
  ctx.fillStyle = SAGE;
  ctx.fillRect(MARGIN, H - 148, 16, 16);
  ctx.font = `600 26px ${SANS}`;
  ctx.fillStyle = INK;
  ctx.fillText("VoiceDumps", MARGIN + 30, H - 134);
  ctx.font = `500 22px ${MONO}`;
  ctx.fillStyle = FAINT;
  ctx.textAlign = "right";
  ctx.fillText("voicedumps.qwee.ai", W - MARGIN, H - 134);
  ctx.textAlign = "left";

  const blob: Blob = await new Promise((resolve, reject) =>
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error("could not encode the image"))),
      "image/png",
    ),
  );
  return new Uint8Array(await blob.arrayBuffer());
}
