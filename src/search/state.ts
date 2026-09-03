import type { SearchHit, SearchWave } from "./ipc";

export type ResultsView =
  | { status: "idle" }
  | { status: "pending"; hits: SearchHit[]; slow: boolean }
  | {
      status: "streaming";
      hits: SearchHit[];
      slow: boolean;
      stage: SearchWave["stage"];
    }
  | { status: "done"; hits: SearchHit[]; stage: SearchWave["stage"] }
  | { status: "failed"; hits: SearchHit[] };

export type SearchState =
  | { mode: "browse"; query: string }
  | {
      mode: "search";
      query: string;
      results: ResultsView;
      focusedIndex: number;
      // Set only by deliberate result navigation. Later waves keep this exact Item focused
      // while every other row remains free to move to its latest rank.
      deliberateFocusPath: string | null;
    };

export const initialSearchState: SearchState = { mode: "browse", query: "" };

export type SearchAction =
  | { type: "activate" }
  | { type: "deactivate" }
  | { type: "queryChanged"; query: string }
  | { type: "focusDelta"; delta: number }
  | { type: "resultsWave"; query: string; wave: SearchWave }
  | { type: "resultsFailed"; query: string }
  | { type: "resultsSlow"; query: string };

export function displayedHits(results: ResultsView): readonly SearchHit[] {
  switch (results.status) {
    case "idle":
      return [];
    case "pending":
    case "streaming":
    case "done":
    case "failed":
      return results.hits;
  }
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function initialResultsFor(query: string, priorHits: SearchHit[]): ResultsView {
  return query.trim() === ""
    ? { status: "idle" }
    : { status: "pending", hits: priorHits, slow: false };
}

export function searchReducer(
  state: SearchState,
  action: SearchAction,
): SearchState {
  switch (action.type) {
    case "activate":
      return state.mode === "search"
        ? state
        : {
            mode: "search",
            query: state.query,
            results: initialResultsFor(state.query, []),
            focusedIndex: 0,
            deliberateFocusPath: null,
          };
    case "deactivate":
      return { mode: "browse", query: state.query };
    case "queryChanged": {
      const prior = state.mode === "search" ? [...displayedHits(state.results)] : [];
      return {
        mode: "search",
        query: action.query,
        results: initialResultsFor(action.query, prior),
        focusedIndex: 0,
        deliberateFocusPath: null,
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
      const focusedIndex = clamp(state.focusedIndex + action.delta, 0, hits.length - 1);
      return {
        ...state,
        focusedIndex,
        deliberateFocusPath: hits[focusedIndex]?.path ?? null,
      };
    }
    case "resultsWave": {
      if (state.mode !== "search" || state.query !== action.query) {
        return state;
      }
      let focusedIndex = 0;
      let deliberateFocusPath = state.deliberateFocusPath;
      if (deliberateFocusPath !== null) {
        const preserved = action.wave.hits.findIndex(
          (hit) => hit.path === deliberateFocusPath,
        );
        focusedIndex =
          preserved >= 0
            ? preserved
            : clamp(state.focusedIndex, 0, Math.max(0, action.wave.hits.length - 1));
        deliberateFocusPath = action.wave.hits[focusedIndex]?.path ?? null;
      }
      const results: ResultsView = action.wave.complete
        ? { status: "done", hits: action.wave.hits, stage: action.wave.stage }
        : {
            status: "streaming",
            hits: action.wave.hits,
            slow: false,
            stage: action.wave.stage,
          };
      return { ...state, results, focusedIndex, deliberateFocusPath };
    }
    case "resultsFailed": {
      if (state.mode !== "search" || state.query !== action.query) {
        return state;
      }
      return {
        ...state,
        results: { status: "failed", hits: [...displayedHits(state.results)] },
      };
    }
    case "resultsSlow": {
      if (state.mode !== "search" || state.query !== action.query) {
        return state;
      }
      if (state.results.status === "pending" || state.results.status === "streaming") {
        return { ...state, results: { ...state.results, slow: true } };
      }
      return state;
    }
  }
}
