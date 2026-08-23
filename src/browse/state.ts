import type { Item, ListErrorPayload } from "../location/schema";

// One variant per real state; `items` is only reachable once `ready`.
export type LoadState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "error"; error: ListErrorPayload }
  | { status: "ready"; items: Item[] };

export interface HistoryEntry {
  location: string;
  focusedPath: string | null;
}

// Single implicit Tab for chunk A, shaped so chunk C can lift it into many Tabs.
export interface BrowseState {
  location: string;
  load: LoadState;
  focusedIndex: number;
  history: HistoryEntry[];
}

export type NavKind = "forward" | "back" | "replace";

export type BrowseAction =
  | { type: "focusDelta"; delta: number }
  | { type: "focusIndex"; index: number }
  | {
      type: "listed";
      location: string;
      items: Item[];
      restorePath: string | null;
      nav: NavKind;
    }
  | { type: "failed"; location: string; error: ListErrorPayload };

export const initialBrowseState: BrowseState = {
  location: "",
  load: { status: "loading" },
  focusedIndex: 0,
  history: [],
};

function clamp(value: number, min: number, max: number): number {
  if (value < min) {
    return min;
  }
  if (value > max) {
    return max;
  }
  return value;
}

function focusedPathOf(state: BrowseState): string | null {
  if (state.load.status !== "ready") {
    return null;
  }
  return state.load.items[state.focusedIndex]?.path ?? null;
}

function withFocus(state: BrowseState, index: number): BrowseState {
  if (state.load.status !== "ready") {
    return state;
  }
  const count = state.load.items.length;
  if (count === 0) {
    return state;
  }
  const next = clamp(index, 0, count - 1);
  return next === state.focusedIndex ? state : { ...state, focusedIndex: next };
}

export function browseReducer(
  state: BrowseState,
  action: BrowseAction,
): BrowseState {
  switch (action.type) {
    case "focusDelta":
      return withFocus(state, state.focusedIndex + action.delta);
    case "focusIndex":
      return withFocus(state, action.index);
    case "listed": {
      const restoredIndex =
        action.restorePath === null
          ? 0
          : action.items.findIndex((item) => item.path === action.restorePath);
      // A committed navigation records history relative to the outgoing state,
      // which still reflects the origin Location until this action lands.
      const history =
        action.nav === "forward"
          ? [
              ...state.history,
              { location: state.location, focusedPath: focusedPathOf(state) },
            ]
          : action.nav === "back"
            ? state.history.slice(0, -1)
            : state.history;
      return {
        location: action.location,
        load: { status: "ready", items: action.items },
        focusedIndex: Math.max(0, restoredIndex),
        history,
      };
    }
    case "failed":
      // A failed navigation from a working view leaves that view intact.
      if (state.load.status === "ready") {
        return state;
      }
      return {
        ...state,
        location: action.location,
        load: { status: "error", error: action.error },
      };
  }
}
