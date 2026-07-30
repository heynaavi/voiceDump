import { Component, type ReactNode } from "react";

type Props = { children: ReactNode };
type State = { error: Error | null };

/**
 * A blank window is the worst possible failure mode for a desktop app — the
 * user has no idea whether it's loading, wedged, or broken. Show the error.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error) {
    console.error("Unhandled UI error", error);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="dot-grid titlebar-pad flex h-full items-center justify-center bg-surface px-10">
        <div className="w-full max-w-[460px] border border-amber bg-panel">
          <p className="micro bg-amber px-3 py-1.5 text-surface">
            FAULT :: INTERFACE
          </p>
          <p className="selectable whitespace-pre-wrap px-3 py-3 font-mono text-[10px] leading-relaxed text-grey">
            {error.message}
          </p>
          <div className="border-t border-hairline-soft px-3 py-2">
            <button
              onClick={() => this.setState({ error: null })}
              className="micro border border-ink px-3 py-1.5 text-ink transition-colors hover:bg-ink hover:text-surface"
            >
              RETRY
            </button>
          </div>
        </div>
      </div>
    );
  }
}
