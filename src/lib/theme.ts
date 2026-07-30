/**
 * Light / dark, chosen by the user rather than by the OS.
 *
 * The design system defines both surfaces — paper pages and forest cover — but
 * they used to be wired to `prefers-color-scheme`, so which one you got was
 * whatever macOS happened to be set to and there was no way to disagree. The
 * tokens now hang off a `data-theme` attribute on <html>, the app opens on
 * paper, and the choice sticks.
 *
 * The pre-paint application lives in index.html: by the time this module is
 * evaluated the first frame has already been painted, so a dark-mode user would
 * see a white flash on every launch if we waited until here.
 */

import { useCallback, useEffect, useState } from "react";

export type Theme = "light" | "dark";

/** Paper, per §3 — the print system this design language came from. */
export const DEFAULT_THEME: Theme = "light";

const STORAGE_KEY = "voicedumps:theme";
const CHANGE_EVENT = "voicedumps:theme-change";

export function storedTheme(): Theme {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    return saved === "dark" || saved === "light" ? saved : DEFAULT_THEME;
  } catch {
    // Storage can throw in a locked-down webview; the default still works.
    return DEFAULT_THEME;
  }
}

export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  root.dataset.theme = theme;
  // Native scrollbars, form controls and the window's own chrome read this —
  // without it they stay light-styled on a forest background.
  root.style.colorScheme = theme;
}

export function setTheme(theme: Theme): void {
  applyTheme(theme);
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // Not fatal: the theme still applies for this session.
  }
  window.dispatchEvent(new CustomEvent<Theme>(CHANGE_EVENT, { detail: theme }));
}

/** The current theme and a toggle. */
export function useTheme(): { theme: Theme; toggle: () => void } {
  const [theme, setLocal] = useState<Theme>(storedTheme);

  useEffect(() => {
    const onChange = (e: Event) => setLocal((e as CustomEvent<Theme>).detail);
    window.addEventListener(CHANGE_EVENT, onChange);
    return () => window.removeEventListener(CHANGE_EVENT, onChange);
  }, []);

  const toggle = useCallback(
    () => setTheme(theme === "dark" ? "light" : "dark"),
    [theme],
  );

  return { theme, toggle };
}
