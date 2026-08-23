import type { SearchHit } from "./ipc";

// The results view of one active search. `pending` carries whatever hits were last
// shown so a newer query never blanks the overlay (progressive results, SPEC §6);
// `done` may hold an empty list, which the overlay renders as "nothing found".
export type ResultsView =
  | { status: "idle" }
  | { status: "pending"; hits: SearchHit[]; slow: boolean }
  | { status: "done"; hits: SearchHit[] }
  | { status: "failed" };

// Per-Tab search state (§4: each Tab retains its own inactive query). Browse Mode
// keeps only the retained query text; Search Mode additionally owns the results
// overlay, the Focused result, and whether reordering has frozen (SPEC §5, §6).
export type SearchState =
  | { mode: "browse"; query: string }
  | {
      mode: "search";
      query: string;
      results: ResultsView;
      focusedIndex: number;
      // Reordering (and auto-focus of the first result) stops once the user starts
      // keyboard navigation within results (SPEC §6); a query edit clears it.
      frozen: boolean;
    };

export const initialSearchState: SearchState = { mode: "browse", query: "" };

export type SearchAction =
  // Enter Search Mode with the retained query (Cmd+L, click, Slash, new Tab).
  | { type: "activate" }
  // Leave Search Mode; the query text stays, inactive (Escape, Reveal).
  | { type: "deactivate" }
  // The user edited the query (implies Search Mode; clears the freeze).
  | { type: "queryChanged"; query: string }
  // Move the Focused result (Up/Down, Ctrl+J/K); freezes reordering.
  | { type: "focusDelta"; delta: number }
  // A search response for `query` landed / failed / crossed the slow threshold.
  | { type: "resultsLanded"; query: string; hits: SearchHit[] }
  | { type: "resultsFailed"; query: string }
  | { type: "resultsSlow"; query: string };

// The hits currently on screen for a results view (empty for idle/failed).
export function displayedHits(results: ResultsView): readonly SearchHit[] {
  if (results.status === "pending" || results.status === "done") {
    return results.hits;
  }
  return [];
}

function clamp(value: number, min: number, max: number): number {
  if (value < min) {
    return min;
  }
  if (value > max) {
    return max;
  }
  return value;
}

// The results view a query starts in: idle for an empty query, otherwise pending
// while carrying the prior hits forward so the overlay never flickers to blank.
function initialResultsFor(query: string, priorHits: SearchHit[]): ResultsView {
  if (query.trim() === "") {
    return { status: "idle" };
  }
  return { status: "pending", hits: priorHits, slow: false };
}

export function searchReducer(
  state: SearchState,
  action: SearchAction,
): SearchState {
  switch (action.type) {
    case "activate": {
      if (state.mode === "search") {
        // Already active: DOM re-selection is handled by the caller, state stands.
        return state;
      }
      return {
        mode: "search",
        query: state.query,
        results: initialResultsFor(state.query, []),
        focusedIndex: 0,
        frozen: false,
      };
    }
    case "deactivate":
      return { mode: "browse", query: state.query };
    case "queryChanged": {
      const prior = state.mode === "search" ? [...displayedHits(state.results)] : [];
      return {
        mode: "search",
        query: action.query,
        results: initialResultsFor(action.query, prior),
        focusedIndex: 0,
        frozen: false,
      };
    }
    case "focusDelta": {
      if (state.mode !== "search") {
        return state;
      }
      const hits = displayedHits(state.results);
      if (hits.length === 0) {
        return state;
      }
      const next = clamp(state.focusedIndex + action.delta, 0, hits.length - 1);
      return { ...state, focusedIndex: next, frozen: true };
    }
    case "resultsLanded": {
      // Stale application is already blocked by the caller's seq guard; the query
      // check drops a response the user has since typed past.
      if (state.mode !== "search" || state.query !== action.query) {
        return state;
      }
      // A frozen list keeps the Focused result put; otherwise the first result is
      // auto-focused (SPEC §6).
      const focusedIndex = state.frozen
        ? clamp(state.focusedIndex, 0, Math.max(0, action.hits.length - 1))
        : 0;
      return {
        ...state,
        results: { status: "done", hits: action.hits },
        focusedIndex,
      };
    }
    case "resultsFailed": {
      if (state.mode !== "search" || state.query !== action.query) {
        return state;
      }
      return { ...state, results: { status: "failed" }, focusedIndex: 0 };
    }
    case "resultsSlow": {
      if (
        state.mode !== "search" ||
        state.query !== action.query ||
        state.results.status !== "pending"
      ) {
        return state;
      }
      return { ...state, results: { ...state.results, slow: true } };
    }
  }
}
