import { browseReducer, type BrowseAction } from "../browse/state";
import { searchReducer, type SearchAction } from "../search/state";
import {
  freshTabId,
  makeTemporaryTab,
  pinnedCount,
  type PinnedTab,
  type Tab,
  type TabId,
  type TemporaryTab,
} from "./model";

// The whole Tab bar: an ordered list (Pinned group first, then Temporary group)
// and exactly one active Tab. Invariants held by every reducer transition: the
// list is never empty, every Pinned Tab precedes every Temporary Tab, and
// `activeId` names a Tab in the list.
export interface TabsState {
  tabs: Tab[];
  activeId: TabId;
}

export type TabsAction =
  | { type: "hydrate"; state: TabsState }
  | { type: "browse"; tabId: TabId; action: BrowseAction }
  | { type: "search"; tabId: TabId; action: SearchAction }
  | {
      type: "create";
      tab: Tab;
      index: number;
      outgoingScrollTop: number;
      nowMs: number;
    }
  | { type: "activate"; tabId: TabId; outgoingScrollTop: number; nowMs: number }
  // Re-stamp the active Temporary Tab's activation time without any scroll change
  // (the window became visible; the DOM already holds its scroll — §2).
  | { type: "touchActive"; nowMs: number }
  | {
      type: "remove";
      tabId: TabId;
      replacement: TemporaryTab;
      outgoingScrollTop: number;
      nowMs: number;
    }
  | { type: "removeExpired"; ids: TabId[] }
  | { type: "pin"; tabId: TabId }
  | { type: "unpin"; tabId: TabId; nowMs: number }
  | { type: "rename"; tabId: TabId; name: string }
  | { type: "reorder"; tabId: TabId; targetIndex: number };

export function createInitialTabsState(nowMs: number): TabsState {
  const tab = makeTemporaryTab({ id: freshTabId(), nowMs, originatorId: null });
  return { tabs: [tab], activeId: tab.id };
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

function withBrowse<T extends Tab>(tab: T, action: BrowseAction): T {
  return { ...tab, browse: browseReducer(tab.browse, action) };
}

function withSearch<T extends Tab>(tab: T, action: SearchAction): T {
  return { ...tab, search: searchReducer(tab.search, action) };
}

function replaceTab(
  tabs: Tab[],
  tabId: TabId,
  update: (tab: Tab) => Tab,
): Tab[] {
  return tabs.map((tab) => (tab.id === tabId ? update(tab) : tab));
}

// Switch the active Tab: snapshot the outgoing Tab's live scroll, stamp the
// incoming Temporary Tab's activation time, and queue its saved scroll for
// restore. Tolerates an `activeId` that no longer names a Tab (used right after
// a removal), in which case there is simply no outgoing Tab to snapshot.
function activateTo(
  tabs: Tab[],
  fromActiveId: TabId,
  targetId: TabId,
  outgoingScrollTop: number,
  nowMs: number,
): TabsState {
  const next = tabs.map((tab) => {
    if (tab.id === fromActiveId && tab.id !== targetId) {
      return withBrowse(tab, { type: "saveScroll", top: outgoingScrollTop });
    }
    if (tab.id === targetId) {
      const stamped =
        tab.kind === "temporary" ? { ...tab, lastActivatedAtMs: nowMs } : tab;
      return withBrowse(stamped, { type: "restoreScroll" });
    }
    return tab;
  });
  return { tabs: next, activeId: targetId };
}

function nextActiveAfterRemoval(
  tabs: Tab[],
  removedIndex: number,
  removed: Tab,
): TabId {
  const first = tabs[0];
  if (first === undefined) {
    throw new Error("nextActiveAfterRemoval requires a non-empty list");
  }
  // The originator is preferred when it is still alive (§4).
  const originatorId =
    removed.kind === "temporary" ? removed.originatorId : null;
  if (originatorId !== null) {
    const originator = tabs.find((tab) => tab.id === originatorId);
    if (originator !== undefined) {
      return originator.id;
    }
  }
  // Otherwise the nearest Tab to the left of the removed slot.
  const leftIndex = clamp(removedIndex - 1, 0, tabs.length - 1);
  return tabs[leftIndex]?.id ?? first.id;
}

export function tabsReducer(state: TabsState, action: TabsAction): TabsState {
  switch (action.type) {
    case "hydrate":
      return action.state;

    case "browse":
      return {
        ...state,
        tabs: replaceTab(state.tabs, action.tabId, (tab) =>
          withBrowse(tab, action.action),
        ),
      };

    case "search":
      return {
        ...state,
        tabs: replaceTab(state.tabs, action.tabId, (tab) =>
          withSearch(tab, action.action),
        ),
      };

    case "create": {
      const index = clamp(action.index, 0, state.tabs.length);
      const tabs = [
        ...state.tabs.slice(0, index),
        action.tab,
        ...state.tabs.slice(index),
      ];
      return activateTo(
        tabs,
        state.activeId,
        action.tab.id,
        action.outgoingScrollTop,
        action.nowMs,
      );
    }

    case "activate": {
      if (!state.tabs.some((tab) => tab.id === action.tabId)) {
        return state;
      }
      return activateTo(
        state.tabs,
        state.activeId,
        action.tabId,
        action.outgoingScrollTop,
        action.nowMs,
      );
    }

    case "touchActive":
      return {
        ...state,
        tabs: state.tabs.map((tab) =>
          tab.id === state.activeId && tab.kind === "temporary"
            ? { ...tab, lastActivatedAtMs: action.nowMs }
            : tab,
        ),
      };

    case "remove": {
      const removedIndex = state.tabs.findIndex(
        (tab) => tab.id === action.tabId,
      );
      if (removedIndex === -1) {
        return state;
      }
      const removed = state.tabs[removedIndex];
      const tabs = state.tabs.filter((tab) => tab.id !== action.tabId);
      if (tabs.length === 0 || removed === undefined) {
        // The last Tab: a clean Temporary Tab takes its place (no tabless window).
        return { tabs: [action.replacement], activeId: action.replacement.id };
      }
      if (action.tabId !== state.activeId) {
        return { tabs, activeId: state.activeId };
      }
      const targetId = nextActiveAfterRemoval(tabs, removedIndex, removed);
      return activateTo(
        tabs,
        state.activeId,
        targetId,
        action.outgoingScrollTop,
        action.nowMs,
      );
    }

    case "removeExpired": {
      if (action.ids.length === 0) {
        return state;
      }
      const drop = new Set<TabId>(action.ids);
      const tabs = state.tabs.filter((tab) => !drop.has(tab.id));
      // The active Tab is never expired, so the list cannot empty here.
      if (tabs.length === 0) {
        return state;
      }
      return { ...state, tabs };
    }

    case "pin": {
      const tab = state.tabs.find((candidate) => candidate.id === action.tabId);
      if (tab === undefined || tab.kind !== "temporary") {
        return state;
      }
      // Only a directory Location can become an Anchor; a Recents Tab cannot be pinned
      // (Pinned Anchors are directory paths in v1). A silent no-op otherwise.
      if (tab.browse.location.kind !== "directory") {
        return state;
      }
      const anchorPath = tab.browse.location.path;
      // Two Pinned Tabs cannot share an Anchor: a silent no-op (§4).
      const anchorTaken = state.tabs.some(
        (candidate) =>
          candidate.kind === "pinned" && candidate.anchorPath === anchorPath,
      );
      if (anchorTaken) {
        return state;
      }
      const pinned: PinnedTab = {
        kind: "pinned",
        id: tab.id,
        anchorPath,
        customName: null,
        browse: tab.browse,
        search: tab.search,
      };
      const without = state.tabs.filter((candidate) => candidate.id !== tab.id);
      const insertAt = pinnedCount(without);
      const tabs = [
        ...without.slice(0, insertAt),
        pinned,
        ...without.slice(insertAt),
      ];
      return { ...state, tabs };
    }

    case "unpin": {
      const tab = state.tabs.find((candidate) => candidate.id === action.tabId);
      if (tab === undefined || tab.kind !== "pinned") {
        return state;
      }
      const temporary: TemporaryTab = {
        kind: "temporary",
        id: tab.id,
        createdAtMs: action.nowMs,
        lastActivatedAtMs: action.nowMs,
        originatorId: null,
        browse: tab.browse,
        search: tab.search,
      };
      const without = state.tabs.filter((candidate) => candidate.id !== tab.id);
      const insertAt = pinnedCount(without);
      const tabs = [
        ...without.slice(0, insertAt),
        temporary,
        ...without.slice(insertAt),
      ];
      return { ...state, tabs };
    }

    case "rename": {
      const name = action.name.trim();
      return {
        ...state,
        tabs: replaceTab(state.tabs, action.tabId, (tab) =>
          tab.kind === "pinned"
            ? { ...tab, customName: name === "" ? null : name }
            : tab,
        ),
      };
    }

    case "reorder": {
      const from = state.tabs.findIndex((tab) => tab.id === action.tabId);
      const moving = state.tabs[from];
      if (from === -1 || moving === undefined) {
        return state;
      }
      const arr = state.tabs.filter((_, index) => index !== from);
      const pinnedAfter = pinnedCount(arr);
      // Insertion stays inside the Tab's own group (cross-group moves are pin /
      // unpin, handled separately).
      const [lo, hi] =
        moving.kind === "pinned"
          ? [0, pinnedAfter]
          : [pinnedAfter, arr.length];
      const dest = clamp(action.targetIndex, lo, hi);
      arr.splice(dest, 0, moving);
      return { ...state, tabs: arr };
    }
  }
}
