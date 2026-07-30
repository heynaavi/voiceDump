import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { DictationOverlay } from "./components/DictationOverlay";
import { ErrorBoundary } from "./components/ErrorBoundary";
import "./styles.css";

// Both windows load this same bundle; the query flag picks which one to mount.
// Sharing the entry point keeps the design tokens and fonts in one place rather
// than duplicating them for a 260px pill.
const isOverlay = new URLSearchParams(window.location.search).has("overlay");

if (isOverlay) {
  // The overlay floats over other apps, so its window is transparent and the
  // usual opaque page background would show as a black rectangle.
  document.documentElement.style.background = "transparent";
  document.body.style.background = "transparent";
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>{isOverlay ? <DictationOverlay /> : <App />}</ErrorBoundary>
  </React.StrictMode>,
);
