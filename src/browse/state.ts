import {
  directoryLocation,
  locationEquals,
  type Location,
} from "../location/location";
import type { RecentsUnavailableReason } from "../location/recents";
import type { Item, ListErrorPayload } from "../location/schema";

// Why a load produced no usable list. The three Spotlight reasons (SPEC §7) plus
// "unknown" for a boundary failure that never resolved to one of them.
export type LoadUnavailableReason = RecentsUnavailableReason | "unknown";

// One variant per real state; `items` is only reachable once `ready`. `unavailable` is
// produced only by Recents (a degraded Spotlight), never by a directory listing (§7).
export type LoadState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "error"; error: ListErrorPayload }
  | { status: "unavailable"; reason: LoadUnavailableReason }
  | { status: "ready"; items: Item[] };

// The outcome of loading a Location: a list of Items, or a degraded Recents state. One
// type so the `listed` transition does its history bookkeeping once for both.
export type LoadResult =
  | { kind: "items"; items: Item[] }
  | { kind: "unavailable"; reason: LoadUnavailableReason };

// How a mouse gesture changes selection. Keyboard uses `focusDelta`/`clearSelection`.
export type SelectMode = "plain" | "range" | "toggle";

// A saved browsing position: the Focused Item, the Selected Items, and the
// scroll offset, all keyed by path so they survive re-listing a Location.
export interface HistoryEntry {
  location: Location;
  focusedPath: string | null;
  selectedPaths: string[];
  scrollTop: number;
}

// Single implicit Tab for chunk A/B, shaped so chunk C can lift it into many Tabs.
export interface BrowseState {
  location: Location;
  load: LoadState;
  // Exactly one Focused Item; `anchorIndex` is the fixed end of a range selection.
  focusedIndex: number;
  anchorIndex: number;
  // Selected Items as a set of row indices. Empty only for an empty Location.
  selected: ReadonlySet<number>;
  history: HistoryEntry[];
  future: HistoryEntry[];
  // Saved scroll offset of this view, retained across Tab switches. The live
  // offset lives in the table; this is snapshotted when the Tab is deactivated.
  scrollTop: number;
  // Scroll offset to apply after a restore listing; `scrollGeneration` bumps once
  // per landed listing (or Tab reactivation) so the table re-applies it exactly once.
  pendingScrollTop: number;
  scrollGeneration: number;
}

// enter: new navigation into a Location. back/forward: history stack moves.
// replace: the initial load, which records no history. reset: like replace but
// also drops both stacks (returning a Pinned Tab to its Anchor).
export type NavKind = "enter" | "back" | "forward" | "replace" | "reset";

export type BrowseAction =
  | { type: "focusDelta"; delta: number; extend: boolean }
  // Move the Focused Item to a specific row without touching the Selected Items — used to
  // keep the Focused Item in sync with Quick Look navigation while preserving the original
  // selection on close (SPEC §9).
  | { type: "refocus"; index: number }
  | { type: "select"; index: number; mode: SelectMode }
  | { type: "clearSelection" }
  | {
      type: "listed";
      location: Location;
      result: LoadResult;
      nav: NavKind;
      originScrollTop: number;
      // A specific path to focus on arrival (a file Reveal focuses its target,
      // §6); null lets history restore or the first row take focus.
      focusPath: string | null;
    }
  // Background re-list of the current Location (Tab switch, §11 revalidation):
  // items are replaced but the Focused Item never moves and history/scroll stand.
  | {
      type: "revalidated";
      location: Location;
      items: Item[];
      // A progressive large-directory completion can still need to land on the file that
      // Search Reveal requested but which was outside the initial prefix. Null preserves
      // the current focused/selected paths during ordinary background revalidation.
      focusPath: string | null;
    }
  // Append the next page of Recents onto the current view (progressive scroll, §7).
  | { type: "recentsAppended"; items: Item[] }
  // Snapshot the live scroll offset into the Tab (on deactivation).
  | { type: "saveScroll"; top: number }
  // Reapply the saved scroll offset without a new listing (Tab reactivation).
  | { type: "restoreScroll" }
  | { type: "failed"; location: Location; error: ListErrorPayload };

export const initialBrowseState: BrowseState = {
  location: directoryLocation(""),
  load: { status: "loading" },
  focusedIndex: 0,
  anchorIndex: 0,
  selected: new Set<number>(),
  history: [],
  future: [],
  scrollTop: 0,
  pendingScrollTop: 0,
  scrollGeneration: 0,
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

function rangeSet(a: number, b: number): Set<number> {
  const lo = Math.min(a, b);
  const hi = Math.max(a, b);
  const set = new Set<number>();
  for (let index = lo; index <= hi; index += 1) {
    set.add(index);
  }
  return set;
}

function focusedPathOf(state: BrowseState): string | null {
  if (state.load.status !== "ready") {
    return null;
  }
  return state.load.items[state.focusedIndex]?.path ?? null;
}

function selectedPathsOf(state: BrowseState): string[] {
  if (state.load.status !== "ready") {
    return [];
  }
  const items = state.load.items;
  const paths: string[] = [];
  for (const index of state.selected) {
    const item = items[index];
    if (item !== undefined) {
      paths.push(item.path);
    }
  }
  return paths;
}

// True when Selected Items holds more than the bare focused row — the case in
// which Escape collapses selection instead of hiding the window (§5).
export function hasSelectionBeyondFocus(state: BrowseState): boolean {
  if (state.load.status !== "ready") {
    return false;
  }
  const selected = state.selected;
  if (selected.size === 0) {
    return false;
  }
  if (selected.size === 1 && selected.has(state.focusedIndex)) {
    return false;
  }
  return true;
}

// Resolve a saved position (or a fresh entry) against the freshly listed items.
function positionFor(
  restore: HistoryEntry | null,
  items: Item[],
): { focusedIndex: number; selected: Set<number>; scrollTop: number } {
  const focusPath = restore?.focusedPath ?? null;
  const focusedIndex =
    focusPath === null
      ? 0
      : Math.max(
          0,
          items.findIndex((item) => item.path === focusPath),
        );
  const selected = new Set<number>();
  if (restore !== null) {
    for (const path of restore.selectedPaths) {
      const index = items.findIndex((item) => item.path === path);
      if (index >= 0) {
        selected.add(index);
      }
    }
  }
  // A fresh Location (or one whose saved selection vanished) collapses onto the
  // Focused Item so there is always at least the bare focused row.
  if (selected.size === 0 && items.length > 0) {
    selected.add(focusedIndex);
  }
  return { focusedIndex, selected, scrollTop: restore?.scrollTop ?? 0 };
}

function applySelect(
  state: BrowseState,
  rawIndex: number,
  mode: SelectMode,
): BrowseState {
  if (state.load.status !== "ready") {
    return state;
  }
  const count = state.load.items.length;
  if (count === 0) {
    return state;
  }
  const index = clamp(rawIndex, 0, count - 1);
  switch (mode) {
    case "plain":
      // A plain click focuses the row and collapses selection onto it.
      return {
        ...state,
        focusedIndex: index,
        anchorIndex: index,
        selected: new Set<number>([index]),
      };
    case "range":
      // Shift+Click extends the range from the fixed anchor to the click.
      return {
        ...state,
        focusedIndex: index,
        selected: rangeSet(state.anchorIndex, index),
      };
    case "toggle": {
      // Cmd+Click flips one row and re-anchors on it.
      const selected = new Set<number>(state.selected);
      if (selected.has(index)) {
        selected.delete(index);
      } else {
        selected.add(index);
      }
      return { ...state, focusedIndex: index, anchorIndex: index, selected };
    }
  }
}

export function browseReducer(
  state: BrowseState,
  action: BrowseAction,
): BrowseState {
  switch (action.type) {
    case "focusDelta": {
      if (state.load.status !== "ready") {
        return state;
      }
      const count = state.load.items.length;
      if (count === 0) {
        return state;
      }
      const next = clamp(state.focusedIndex + action.delta, 0, count - 1);
      if (action.extend) {
        // Shift+arrows extend the range from the fixed anchor to the new focus.
        return {
          ...state,
          focusedIndex: next,
          selected: rangeSet(state.anchorIndex, next),
        };
      }
      // Plain arrows collapse selection onto the focused row and reset the anchor.
      return {
        ...state,
        focusedIndex: next,
        anchorIndex: next,
        selected: new Set<number>([next]),
      };
    }
    case "refocus": {
      if (state.load.status !== "ready") {
        return state;
      }
      const count = state.load.items.length;
      if (count === 0) {
        return state;
      }
      const index = clamp(action.index, 0, count - 1);
      // Focus (and the range anchor) move to the row; the Selected Items stand, so closing
      // Quick Look restores the original selection with the Focused Item on the last file.
      return { ...state, focusedIndex: index, anchorIndex: index };
    }
    case "select":
      return applySelect(state, action.index, action.mode);
    case "clearSelection": {
      if (state.load.status !== "ready") {
        return state;
      }
      return {
        ...state,
        anchorIndex: state.focusedIndex,
        selected: new Set<number>([state.focusedIndex]),
      };
    }
    case "listed": {
      const items =
        action.result.kind === "items" ? action.result.items : [];
      const outgoing: HistoryEntry = {
        location: state.location,
        focusedPath: focusedPathOf(state),
        selectedPaths: selectedPathsOf(state),
        scrollTop: action.originScrollTop,
      };
      let history = state.history;
      let future = state.future;
      let restore: HistoryEntry | null = null;
      switch (action.nav) {
        case "enter":
          // A new navigation records the origin and drops any redo path.
          history = [...state.history, outgoing];
          future = [];
          break;
        case "back":
          restore = state.history[state.history.length - 1] ?? null;
          history = state.history.slice(0, -1);
          future = [...state.future, outgoing];
          break;
        case "forward":
          restore = state.future[state.future.length - 1] ?? null;
          future = state.future.slice(0, -1);
          history = [...state.history, outgoing];
          break;
        case "replace":
          break;
        case "reset":
          // Returning a Pinned Tab to its Anchor: no history in either direction.
          history = [];
          future = [];
          break;
      }
      // A Reveal target overrides history restoration: focus (and select) it.
      const position =
        action.focusPath !== null
          ? positionFor(
              {
                location: action.location,
                focusedPath: action.focusPath,
                selectedPaths: [action.focusPath],
                scrollTop: 0,
              },
              items,
            )
          : positionFor(restore, items);
      const load: LoadState =
        action.result.kind === "items"
          ? { status: "ready", items }
          : { status: "unavailable", reason: action.result.reason };
      return {
        location: action.location,
        load,
        focusedIndex: position.focusedIndex,
        anchorIndex: position.focusedIndex,
        selected: position.selected,
        history,
        future,
        scrollTop: position.scrollTop,
        pendingScrollTop: position.scrollTop,
        scrollGeneration: state.scrollGeneration + 1,
      };
    }
    case "revalidated": {
      // A stale background result for a Location we have since left is dropped.
      if (
        state.load.status !== "ready" ||
        !locationEquals(action.location, state.location)
      ) {
        return state;
      }
      // Re-resolve the Focused and Selected Items by path so the focused row
      // keeps its identity even as indices shift; scroll and history untouched.
      const restore: HistoryEntry =
        action.focusPath === null
          ? {
              location: state.location,
              focusedPath: focusedPathOf(state),
              selectedPaths: selectedPathsOf(state),
              scrollTop: state.scrollTop,
            }
          : {
              location: state.location,
              focusedPath: action.focusPath,
              selectedPaths: [action.focusPath],
              scrollTop: 0,
            };
      const position = positionFor(restore, action.items);
      return {
        ...state,
        load: { status: "ready", items: action.items },
        focusedIndex: position.focusedIndex,
        anchorIndex: position.focusedIndex,
        selected: position.selected,
      };
    }
    case "recentsAppended": {
      // Append the next Recents page onto the current view without disturbing the
      // Focused Item, selection, or scroll (indices are stable — rows only grow at the
      // end). Guarded to the Recents view so a stale page can never land elsewhere (§7).
      if (state.load.status !== "ready" || state.location.kind !== "recents") {
        return state;
      }
      const seen = new Set(state.load.items.map((item) => item.path));
      const fresh = action.items.filter((item) => !seen.has(item.path));
      if (fresh.length === 0) {
        return state;
      }
      return {
        ...state,
        load: { status: "ready", items: [...state.load.items, ...fresh] },
      };
    }
    case "saveScroll":
      return { ...state, scrollTop: action.top };
    case "restoreScroll":
      return {
        ...state,
        pendingScrollTop: state.scrollTop,
        scrollGeneration: state.scrollGeneration + 1,
      };
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
