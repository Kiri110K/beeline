import {
  directoryLocation,
  locationEquals,
  type Location,
} from "../location/location";
import type { RecentsUnavailableReason } from "../location/recents";
import type {
  Item,
  ListingSessionId,
  ListErrorPayload,
  ResolvedPath,
} from "../location/schema";

export type LoadUnavailableReason = RecentsUnavailableReason | "unknown";

export interface LoadedItems {
  items: Item[];
  offset: number;
  total: number;
  sessionId: ListingSessionId | null;
  focusIndex: number | null;
  selected: ResolvedPath[];
}

export type ReadyLoadState = {
  status: "ready";
  items: Item[];
  offset: number;
  total: number;
  sessionId: ListingSessionId | null;
};

export type LoadState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "error"; error: ListErrorPayload }
  | { status: "unavailable"; reason: LoadUnavailableReason }
  | ReadyLoadState;

export type LoadResult =
  | { kind: "items"; load: LoadedItems }
  | { kind: "unavailable"; reason: LoadUnavailableReason };

export type SelectMode = "plain" | "range" | "toggle";

export interface HistoryEntry {
  location: Location;
  focusedPath: string | null;
  selectedPaths: string[];
  scrollTop: number;
}

export interface BrowseState {
  location: Location;
  load: LoadState;
  focusedIndex: number;
  focusedPath: string | null;
  anchorIndex: number;
  selected: ReadonlySet<number>;
  // Paths survive outside the metadata window, so operations and history still address the
  // exact Selected Items after their rows have scrolled out of WebContent memory.
  selectedPaths: ReadonlyMap<number, string>;
  history: HistoryEntry[];
  future: HistoryEntry[];
  scrollTop: number;
  pendingScrollTop: number;
  scrollGeneration: number;
}

export type NavKind = "enter" | "back" | "forward" | "replace" | "reset";

export type BrowseAction =
  | { type: "focusDelta"; delta: number; extend: boolean }
  | { type: "refocus"; index: number; path: string }
  | { type: "select"; index: number; mode: SelectMode }
  | { type: "clearSelection" }
  | {
      type: "listed";
      location: Location;
      result: LoadResult;
      nav: NavKind;
      originScrollTop: number;
      focusPath: string | null;
      focusScrollTop: number | null;
    }
  | {
      type: "revalidated";
      location: Location;
      load: LoadedItems;
      focusPath: string | null;
    }
  | {
      type: "windowLoaded";
      sessionId: ListingSessionId;
      items: Item[];
      offset: number;
      total: number;
    }
  | {
      type: "selectionPathsResolved";
      sessionId: ListingSessionId;
      selected: ResolvedPath[];
    }
  | { type: "recentsAppended"; items: Item[] }
  | { type: "saveScroll"; top: number }
  | { type: "restoreScroll" }
  | { type: "failed"; location: Location; error: ListErrorPayload };

export const initialBrowseState: BrowseState = {
  location: directoryLocation(""),
  load: { status: "loading" },
  focusedIndex: 0,
  focusedPath: null,
  anchorIndex: 0,
  selected: new Set<number>(),
  selectedPaths: new Map<number, string>(),
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

export function itemAt(load: ReadyLoadState, index: number): Item | undefined {
  return load.items[index - load.offset];
}

function itemAtLoaded(load: LoadedItems, index: number): Item | undefined {
  return load.items[index - load.offset];
}

export function focusedItemOf(state: BrowseState): Item | undefined {
  return state.load.status === "ready"
    ? itemAt(state.load, state.focusedIndex)
    : undefined;
}

export function selectedPathsOf(state: BrowseState): string[] {
  const paths: string[] = [];
  const indices = [...state.selected].sort((a, b) => a - b);
  for (const index of indices) {
    const path = state.selectedPaths.get(index);
    if (path !== undefined) {
      paths.push(path);
    }
  }
  return paths;
}

function pathsForSelection(
  state: BrowseState,
  selected: ReadonlySet<number>,
): Map<number, string> {
  const paths = new Map<number, string>();
  for (const index of selected) {
    const path =
      state.selectedPaths.get(index) ??
      (state.load.status === "ready" ? itemAt(state.load, index)?.path : undefined);
    if (path !== undefined) {
      paths.set(index, path);
    }
  }
  return paths;
}

export function hasSelectedItems(state: BrowseState): boolean {
  return state.load.status === "ready" && state.selected.size > 0;
}

interface Position {
  focusedIndex: number;
  focusedPath: string | null;
  selected: Set<number>;
  selectedPaths: Map<number, string>;
  scrollTop: number;
}

function positionFor(restore: HistoryEntry | null, load: LoadedItems): Position {
  const requestedFocusPath = restore?.focusedPath ?? null;
  const localFocusIndex =
    requestedFocusPath === null
      ? -1
      : load.items.findIndex((item) => item.path === requestedFocusPath);
  const focusedIndex =
    load.focusIndex ?? (localFocusIndex >= 0 ? load.offset + localFocusIndex : 0);
  const localFocused = itemAtLoaded(load, focusedIndex);
  const focusedPath =
    load.focusIndex !== null && requestedFocusPath !== null
      ? requestedFocusPath
      : (localFocused?.path ?? null);

  const selected = new Set<number>();
  const selectedPaths = new Map<number, string>();
  for (const resolved of load.selected) {
    selected.add(resolved.index);
    selectedPaths.set(resolved.index, resolved.path);
  }
  if (selected.size === 0 && load.total > 0) {
    selected.add(focusedIndex);
    const path = focusedPath ?? localFocused?.path;
    if (path !== undefined) {
      selectedPaths.set(focusedIndex, path);
    }
  }
  return {
    focusedIndex,
    focusedPath,
    selected,
    selectedPaths,
    scrollTop: restore?.scrollTop ?? 0,
  };
}

function applySelect(
  state: BrowseState,
  rawIndex: number,
  mode: SelectMode,
): BrowseState {
  if (state.load.status !== "ready" || state.load.total === 0) {
    return state;
  }
  const index = clamp(rawIndex, 0, state.load.total - 1);
  const item = itemAt(state.load, index);
  if (item === undefined) {
    return state;
  }
  switch (mode) {
    case "plain":
      return {
        ...state,
        focusedIndex: index,
        focusedPath: item.path,
        anchorIndex: index,
        selected: new Set<number>([index]),
        selectedPaths: new Map<number, string>([[index, item.path]]),
      };
    case "range": {
      const selected = rangeSet(state.anchorIndex, index);
      return {
        ...state,
        focusedIndex: index,
        focusedPath: item.path,
        selected,
        selectedPaths: pathsForSelection(state, selected),
      };
    }
    case "toggle": {
      const selected = new Set<number>(state.selected);
      if (selected.has(index)) {
        selected.delete(index);
      } else {
        selected.add(index);
      }
      const selectedPaths = pathsForSelection(state, selected);
      if (selected.has(index)) {
        selectedPaths.set(index, item.path);
      }
      return {
        ...state,
        focusedIndex: index,
        focusedPath: item.path,
        anchorIndex: index,
        selected,
        selectedPaths,
      };
    }
  }
}

export function browseReducer(
  state: BrowseState,
  action: BrowseAction,
): BrowseState {
  switch (action.type) {
    case "focusDelta": {
      if (state.load.status !== "ready" || state.load.total === 0) {
        return state;
      }
      const next = clamp(
        state.focusedIndex + action.delta,
        0,
        state.load.total - 1,
      );
      const item = itemAt(state.load, next);
      if (item === undefined) {
        return state;
      }
      if (action.extend) {
        const selected = rangeSet(state.anchorIndex, next);
        const selectedPaths = pathsForSelection(state, selected);
        selectedPaths.set(next, item.path);
        return {
          ...state,
          focusedIndex: next,
          focusedPath: item.path,
          selected,
          selectedPaths,
        };
      }
      return {
        ...state,
        focusedIndex: next,
        focusedPath: item.path,
        anchorIndex: next,
        selected: new Set<number>([next]),
        selectedPaths: new Map<number, string>([[next, item.path]]),
      };
    }
    case "refocus": {
      if (state.load.status !== "ready" || state.load.total === 0) {
        return state;
      }
      const index = clamp(action.index, 0, state.load.total - 1);
      return {
        ...state,
        focusedIndex: index,
        focusedPath: action.path,
        anchorIndex: index,
      };
    }
    case "select":
      return applySelect(state, action.index, action.mode);
    case "clearSelection": {
      if (state.load.status !== "ready" || state.focusedPath === null) {
        return state;
      }
      return {
        ...state,
        anchorIndex: state.focusedIndex,
        // Focus and Selected Items are independent (§5). Escape clears the selection
        // layer while leaving the Focused Item available for keyboard navigation; this
        // is also how Cmd+K reaches the application-action menu.
        selected: new Set<number>(),
        selectedPaths: new Map<number, string>(),
      };
    }
    case "listed": {
      const outgoing: HistoryEntry = {
        location: state.location,
        focusedPath: state.focusedPath,
        selectedPaths: selectedPathsOf(state),
        scrollTop: action.originScrollTop,
      };
      let history = state.history;
      let future = state.future;
      let restore: HistoryEntry | null = null;
      switch (action.nav) {
        case "enter":
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
          history = [];
          future = [];
          break;
      }
      if (action.result.kind === "unavailable") {
        return {
          location: action.location,
          load: { status: "unavailable", reason: action.result.reason },
          focusedIndex: 0,
          focusedPath: null,
          anchorIndex: 0,
          selected: new Set<number>(),
          selectedPaths: new Map<number, string>(),
          history,
          future,
          scrollTop: 0,
          pendingScrollTop: 0,
          scrollGeneration: state.scrollGeneration + 1,
        };
      }
      const requestedRestore: HistoryEntry | null =
        action.focusPath === null
          ? restore
          : {
              location: action.location,
              focusedPath: action.focusPath,
              selectedPaths: [action.focusPath],
              scrollTop: 0,
            };
      const position = positionFor(requestedRestore, action.result.load);
      const scrollTop = action.focusScrollTop ?? position.scrollTop;
      return {
        location: action.location,
        load: {
          status: "ready",
          items: action.result.load.items,
          offset: action.result.load.offset,
          total: action.result.load.total,
          sessionId: action.result.load.sessionId,
        },
        focusedIndex: position.focusedIndex,
        focusedPath: position.focusedPath,
        anchorIndex: position.focusedIndex,
        selected: position.selected,
        selectedPaths: position.selectedPaths,
        history,
        future,
        scrollTop,
        pendingScrollTop: scrollTop,
        scrollGeneration: state.scrollGeneration + 1,
      };
    }
    case "revalidated": {
      if (
        state.load.status !== "ready" ||
        !locationEquals(action.location, state.location)
      ) {
        return state;
      }
      const restore: HistoryEntry =
        action.focusPath === null
          ? {
              location: state.location,
              focusedPath: state.focusedPath,
              selectedPaths: selectedPathsOf(state),
              scrollTop: state.scrollTop,
            }
          : {
              location: state.location,
              focusedPath: action.focusPath,
              selectedPaths: [action.focusPath],
              scrollTop: 0,
            };
      const position = positionFor(restore, action.load);
      return {
        ...state,
        load: {
          status: "ready",
          items: action.load.items,
          offset: action.load.offset,
          total: action.load.total,
          sessionId: action.load.sessionId,
        },
        focusedIndex: position.focusedIndex,
        focusedPath: position.focusedPath,
        anchorIndex: position.focusedIndex,
        selected: position.selected,
        selectedPaths: position.selectedPaths,
      };
    }
    case "windowLoaded":
      if (
        state.load.status !== "ready" ||
        state.load.sessionId !== action.sessionId
      ) {
        return state;
      }
      return {
        ...state,
        load: {
          status: "ready",
          items: action.items,
          offset: action.offset,
          total: action.total,
          sessionId: action.sessionId,
        },
      };
    case "selectionPathsResolved": {
      if (
        state.load.status !== "ready" ||
        state.load.sessionId !== action.sessionId
      ) {
        return state;
      }
      const selectedPaths = new Map(state.selectedPaths);
      for (const resolved of action.selected) {
        if (state.selected.has(resolved.index)) {
          selectedPaths.set(resolved.index, resolved.path);
        }
      }
      return { ...state, selectedPaths };
    }
    case "recentsAppended": {
      if (
        state.load.status !== "ready" ||
        state.location.kind !== "recents" ||
        state.load.sessionId !== null
      ) {
        return state;
      }
      const seen = new Set(state.load.items.map((item) => item.path));
      const fresh = action.items.filter((item) => !seen.has(item.path));
      if (fresh.length === 0) {
        return state;
      }
      const items = [...state.load.items, ...fresh];
      return {
        ...state,
        load: { ...state.load, items, total: items.length },
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
