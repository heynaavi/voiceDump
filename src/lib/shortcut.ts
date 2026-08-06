/**
 * The dictation chord, as the window understands it.
 *
 * The wire format is Rust's: modifier names joined with `+`, in a fixed order.
 * See `src-tauri/src/shortcut.rs` for why the chord may only contain modifiers
 * — the short version is that the keyboard is watched without being consumed,
 * so a letter in the chord would also reach whatever you are dictating into.
 */

/** Canonical order — the order these keys sit on the keyboard, left to right. */
const ORDER = ["globe", "control", "option", "shift", "command"] as const;

export type Key = (typeof ORDER)[number];

/**
 * What dictation is bound to until somebody changes it. Mirrors
 * `shortcut::DEFAULT` in Rust, and lives here rather than at each call site
 * because the name a key is *stored* as and the name it is *printed* as are
 * different, and only this file knows both.
 *
 * The distinction has already cost one bug. A screen defaulted to `"fn"` —
 * which is what [`GLYPH`] prints for this key, not what [`parse`] answers to —
 * so `glyphs("fn")` returned an empty string and a heading rendered as "Hold
 * and talk" with a hole where the key should be. Typed as `Key`, that line
 * would not have compiled.
 */
export const DEFAULT_CHORD: Key = "globe";

/**
 * What each key is printed as on the caps.
 *
 * The globe key is "fn" rather than 🌐 on purpose: the emoji is the only thing
 * on this screen that arrives in full colour, and it lands next to a sage dot
 * and a monochrome strip looking like it wandered in from another application.
 * Apple prints both on the key.
 */
const GLYPH: Record<Key, string> = {
  globe: "fn",
  control: "⌃",
  option: "⌥",
  shift: "⇧",
  command: "⌘",
};

const WORD: Record<Key, string> = {
  globe: "Globe",
  control: "Control",
  option: "Option",
  shift: "Shift",
  command: "Command",
};

/** The keys a stored chord names, in canonical order, ignoring anything else. */
export function parse(chord: string): Key[] {
  const named = new Set(chord.split("+").map((p) => p.trim().toLowerCase()));
  return ORDER.filter((k) => named.has(k));
}

export const toChord = (keys: Key[]): string =>
  ORDER.filter((k) => keys.includes(k)).join("+");

/** "⌃ ⌥" — what the key caps say, for the readout. */
export const glyphs = (chord: string): string =>
  parse(chord)
    .map((k) => GLYPH[k])
    .join(" ");

/** "Control + Option" — for tooltips and anywhere glyphs would be a riddle. */
export const words = (chord: string): string =>
  parse(chord)
    .map((k) => WORD[k])
    .join(" + ");

/**
 * The modifiers held down during a keyboard event.
 *
 * The globe key is deliberately absent: WKWebView never reports it. It is not a
 * standard modifier, produces no `keydown`, and sets none of the four flags
 * below — which is why it is offered as a preset rather than recorded.
 */
export function held(e: KeyboardEvent): Key[] {
  const keys: Key[] = [];
  if (e.ctrlKey) keys.push("control");
  if (e.altKey) keys.push("option");
  if (e.shiftKey) keys.push("shift");
  if (e.metaKey) keys.push("command");
  return keys;
}

/** Whether a key event is a modifier being pressed rather than a real key. */
export const isModifier = (e: KeyboardEvent): boolean =>
  ["Control", "Alt", "Shift", "Meta"].includes(e.key);

/**
 * Why a set of keys can't be the chord, or null if it can.
 *
 * Mirrors the rule the backend enforces, so the recorder can explain the
 * refusal while the user is still holding the keys rather than after a rejected
 * round trip.
 */
export function refusal(keys: Key[]): string | null {
  if (keys.length === 0) return null;
  if (keys.length === 1 && keys[0] !== "globe") {
    return `${WORD[keys[0]]} on its own is held down at the start of every ${GLYPH[keys[0]]}-key shortcut. Add another key.`;
  }
  return null;
}
