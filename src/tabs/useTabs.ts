import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { openPath, primaryActionFor } from "../browse/actions";
import {
  hasSelectionBeyondFocus,
  initialBrowseState,
  type BrowseState,
  type LoadResult,
  type NavKind,
  type SelectMode,
} from "../browse/state";
import { listLocation } from "../location/ipc";
import {
  directoryLocation,
  recentsLocation,
  type Location,
} from "../location/location";
import { getRecents } from "../location/recents";
import type { Item } from "../location/schema";
import {
  recordVisit,
  searchNameIndex,
  type SearchHit,
} from "../search/ipc";
import {
  displayedHits,
  initialSearchState,
  type SearchState,
} from "../search/state";
import {
  copyToClipboard,
  recordTelemetry,
  reportShellError,
  requestHide,
  subscribeRecentsUpdated,
  subscribeWindowShown,
} from "../shell";
import { defaultEntryPoint } from "./entryPoint";
import {
  BACKGROUND_RESET_MS,
  excursionsToReset,
  expiredTemporaryIds,
} from "./lifecycle";
import {
  freshTabId,
  isOnExcursion,
  isPinned,
  makeTemporaryTab,
  parentPath,
  pinnedCount,
  type PinnedTab,
  type Tab,
  type TabId,
} from "./model";
import { loadPinnedTabs, savePinnedTabs } from "./persistence";
import {
  createInitialTabsState,
  tabsReducer,
  type TabsState,
} from "./state";

// Search tuning (SPEC §6, §10). The overlay shows the top ranked hits; a query is
// "slow" once it is still in flight after this many ms; telemetry is sampled to
// stay cheap (recorded when a query is slow-ish, else every Nth query).
const SEARCH_LIMIT = 50;
const SEARCH_SLOW_MS = 200;
const SEARCH_TELEMETRY_MS_THRESHOLD = 25;
const SEARCH_TELEMETRY_SAMPLE = 20;

// Recents loads and scrolls in pages of this size (SPEC §7): the first page paints from
// the cache, later pages arrive on scroll with no hard cap.
const RECENTS_BATCH = 100;

// The kind of Tab a group boundary drop lands in — the signal that a drag
// crossed the Pinned/Temporary divide and must pin or unpin (§4, point 7).
export type TabGroup = "pinned" | "temporary";

export interface Tabs {
  state: TabsState;
  activeTab: Tab;
  activeBrowse: BrowseState;
  activeSearch: SearchState;
  // Browse interactions, always aimed at the active Tab.
  select: (index: number, mode: SelectMode) => void;
  activateItem: (item: Item) => void;
  onScrollTop: (top: number) => void;
  // Pull the next page of Recents when the table nears its end (§7).
  loadMoreRecents: () => void;
  // Search interactions on the active Tab (§5, §6).
  setInputEl: (element: HTMLInputElement | null) => void;
  activateSearch: () => void;
  deactivateSearch: () => void;
  changeQuery: (text: string) => void;
  focusResultDelta: (delta: number) => void;
  revealFocused: () => void;
  revealResultAt: (index: number) => void;
  // Tab strip interactions.
  clickTab: (id: TabId) => void;
  newTemporaryTab: () => void;
  removeTab: (id: TabId) => void;
  pinTab: (id: TabId) => void;
  unpinTab: (id: TabId) => void;
  renameTab: (id: TabId, name: string) => void;
  copyLocation: (id: TabId) => void;
  reorderTab: (id: TabId, targetIndex: number) => void;
  dropOnGroup: (id: TabId, group: TabGroup, targetIndex: number) => void;
}

function isEditableTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.tagName === "INPUT" || target.isContentEditable)
  );
}

function tabById(state: TabsState, id: TabId): Tab | undefined {
  return state.tabs.find((tab) => tab.id === id);
}

// Whether `target` is the Anchor itself or sits below it (§4 Tab routing).
function isAtOrBelow(target: string, anchor: string): boolean {
  if (target === anchor) {
    return true;
  }
  const prefix = anchor === "/" ? "/" : `${anchor}/`;
  return target.startsWith(prefix);
}

// The most specific Pinned Anchor that covers `target`, or undefined when no
// Anchor is an ancestor-or-equal of it (§4).
function mostSpecificAnchor(
  tabs: readonly Tab[],
  target: string,
): PinnedTab | undefined {
  let best: PinnedTab | undefined;
  for (const tab of tabs) {
    if (tab.kind === "pinned" && isAtOrBelow(target, tab.anchorPath)) {
      if (best === undefined || tab.anchorPath.length > best.anchorPath.length) {
        best = tab;
      }
    }
  }
  return best;
}

function fireTelemetry(name: string, fields: Record<string, unknown>): void {
  void recordTelemetry(name, fields).match(() => undefined, reportShellError);
}

export function useTabs(): Tabs {
  // The opening Tab is stamped at 0; the first show (or navigation) re-stamps it,
  // and it is the active Tab so lifetime expiry never touches it meanwhile.
  const [state, dispatch] = useReducer(tabsReducer, 0, createInitialTabsState);

  // Latest state and live active-Tab scroll for event handlers that must not
  // close over a stale snapshot.
  const stateRef = useRef(state);
  useEffect(() => {
    stateRef.current = state;
  }, [state]);
  const scrollTopRef = useRef(0);

  // Per-Tab Recents paging: the full cached count and whether a page fetch is in flight,
  // so scroll-to-load never over-requests (§7). Loaded count is the view's item length.
  const recentsMetaRef = useRef(
    new Map<TabId, { total: number; loading: boolean }>(),
  );

  // The Navigation Input DOM node, so the controller can focus and select the
  // retained query when Search Mode activates (§5). Registered via a callback ref
  // (never read during render) and used only in effects and event handlers.
  const inputRef = useRef<HTMLInputElement | null>(null);
  const setInputEl = useCallback((element: HTMLInputElement | null): void => {
    inputRef.current = element;
  }, []);

  // Per-Tab monotonic request id: only the newest listing of a given Tab may
  // commit, so a background revalidation never clobbers a live navigation (§10).
  const seqRef = useRef(new Map<TabId, number>());
  const nextSeq = useCallback((id: TabId): number => {
    const seq = (seqRef.current.get(id) ?? 0) + 1;
    seqRef.current.set(id, seq);
    return seq;
  }, []);

  // Per-Tab search request id (cancel-on-newer, §10) and the pending slow-line
  // timer per Tab, plus a sampling counter for search telemetry.
  const searchSeqRef = useRef(new Map<TabId, number>());
  const slowTimerRef = useRef(new Map<TabId, ReturnType<typeof setTimeout>>());
  const searchCountRef = useRef(0);

  const clearSlowTimer = useCallback((id: TabId): void => {
    const timer = slowTimerRef.current.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      slowTimerRef.current.delete(id);
    }
  }, []);

  const navigate = useCallback(
    (
      tabId: TabId,
      target: Location,
      nav: NavKind,
      focusPath?: string,
      forceRecord = false,
    ): void => {
      const seq = nextSeq(tabId);
      const current = stateRef.current;
      const originScrollTop =
        tabId === current.activeId
          ? scrollTopRef.current
          : (tabById(current, tabId)?.browse.scrollTop ?? 0);
      // Commit a landed load. The Location entered is recorded (SPEC §6) only for a
      // directory; programmatic `replace` loads are silent unless forced by a Reveal.
      const commit = (location: Location, result: LoadResult): void => {
        if (seq !== seqRef.current.get(tabId)) {
          return;
        }
        dispatch({
          type: "browse",
          tabId,
          action: {
            type: "listed",
            location,
            result,
            nav,
            originScrollTop,
            focusPath: focusPath ?? null,
          },
        });
        if (location.kind === "directory" && (forceRecord || nav !== "replace")) {
          void recordVisit(location.path, "entered_location").match(
            () => undefined,
            reportShellError,
          );
        }
      };

      if (target.kind === "recents") {
        // A fresh Recents load resets this Tab's paging cursor (§7).
        recentsMetaRef.current.set(tabId, { total: 0, loading: false });
        void getRecents(0, RECENTS_BATCH).match(
          (response) => {
            if (seq !== seqRef.current.get(tabId)) {
              return;
            }
            switch (response.state) {
              case "ok":
                recentsMetaRef.current.set(tabId, {
                  total: response.total,
                  loading: false,
                });
                commit(recentsLocation, { kind: "items", items: response.items });
                break;
              case "empty":
                recentsMetaRef.current.set(tabId, { total: 0, loading: false });
                commit(recentsLocation, { kind: "items", items: [] });
                break;
              case "spotlight_unavailable":
                commit(recentsLocation, {
                  kind: "unavailable",
                  reason: response.reason,
                });
                break;
            }
          },
          (error) => {
            reportShellError(error);
            commit(recentsLocation, { kind: "unavailable", reason: "unknown" });
          },
        );
        return;
      }

      void listLocation(target.path).match(
        (response) => {
          commit(directoryLocation(response.path), {
            kind: "items",
            items: response.items,
          });
        },
        (error) => {
          if (seq !== seqRef.current.get(tabId)) {
            return;
          }
          dispatch({
            type: "browse",
            tabId,
            action: { type: "failed", location: target, error },
          });
        },
      );
    },
    [nextSeq],
  );

  // Re-list the current Location in the background, keeping the Focused Item put. A
  // Recents revalidation re-pulls the first page (resetting the paging cursor); a
  // Spotlight-unavailable revalidation keeps the cached view, like a failed relist.
  const revalidate = useCallback(
    (tabId: TabId, location: Location): void => {
      const seq = nextSeq(tabId);
      if (location.kind === "recents") {
        void getRecents(0, RECENTS_BATCH).match((response) => {
          if (seq !== seqRef.current.get(tabId)) {
            return;
          }
          if (response.state === "ok" || response.state === "empty") {
            const total = response.state === "ok" ? response.total : 0;
            const items = response.state === "ok" ? response.items : [];
            recentsMetaRef.current.set(tabId, { total, loading: false });
            dispatch({
              type: "browse",
              tabId,
              action: { type: "revalidated", location, items },
            });
          }
        }, reportShellError);
        return;
      }
      void listLocation(location.path).match(
        (response) => {
          if (seq !== seqRef.current.get(tabId)) {
            return;
          }
          dispatch({
            type: "browse",
            tabId,
            action: {
              type: "revalidated",
              location: directoryLocation(response.path),
              items: response.items,
            },
          });
        },
        // A failed revalidation keeps the cached listing on screen.
        () => undefined,
      );
    },
    [nextSeq],
  );

  // Pull the next Recents page when the table nears its end (§7). No-op unless the active
  // Tab is a ready Recents view with more cached rows than shown and no fetch in flight.
  const loadMoreRecents = useCallback((): void => {
    const current = stateRef.current;
    const active = tabById(current, current.activeId);
    if (
      active === undefined ||
      active.browse.location.kind !== "recents" ||
      active.browse.load.status !== "ready"
    ) {
      return;
    }
    const tabId = active.id;
    const meta = recentsMetaRef.current.get(tabId) ?? { total: 0, loading: false };
    const loaded = active.browse.load.items.length;
    if (meta.loading || loaded >= meta.total) {
      return;
    }
    recentsMetaRef.current.set(tabId, { ...meta, loading: true });
    void getRecents(loaded, RECENTS_BATCH).match(
      (response) => {
        const currentMeta = recentsMetaRef.current.get(tabId);
        const total =
          response.state === "ok" ? response.total : (currentMeta?.total ?? 0);
        recentsMetaRef.current.set(tabId, { total, loading: false });
        if (response.state === "ok") {
          dispatch({
            type: "browse",
            tabId,
            action: { type: "recentsAppended", items: response.items },
          });
        }
      },
      (error) => {
        reportShellError(error);
        const currentMeta = recentsMetaRef.current.get(tabId);
        if (currentMeta !== undefined) {
          recentsMetaRef.current.set(tabId, { ...currentMeta, loading: false });
        }
      },
    );
  }, []);

  const openFile = useCallback((path: string): void => {
    void openPath(path).match(() => {
      fireTelemetry("file_opened", { path });
      void recordVisit(path, "opened_file").match(() => undefined, reportShellError);
    }, reportShellError);
  }, []);

  const activateItem = useCallback(
    (item: Item): void => {
      const action = primaryActionFor(item);
      switch (action.kind) {
        case "enter":
          navigate(
            stateRef.current.activeId,
            directoryLocation(action.path),
            "enter",
          );
          break;
        case "open":
          openFile(action.path);
          break;
      }
    },
    [navigate, openFile],
  );

  const select = useCallback((index: number, mode: SelectMode): void => {
    dispatch({
      type: "browse",
      tabId: stateRef.current.activeId,
      action: { type: "select", index, mode },
    });
  }, []);

  const onScrollTop = useCallback((top: number): void => {
    scrollTopRef.current = top;
  }, []);

  const focusDelta = useCallback((delta: number, extend: boolean): void => {
    dispatch({
      type: "browse",
      tabId: stateRef.current.activeId,
      action: { type: "focusDelta", delta, extend },
    });
  }, []);

  // Sampled search telemetry: record only when a query was slow-ish or on every
  // Nth query, so the hot typing path stays cheap (§10, telemetry point 10).
  const fireSearchTelemetry = useCallback(
    (queryLen: number, results: number, durationMs: number): void => {
      searchCountRef.current += 1;
      const duration = Math.round(durationMs);
      if (
        duration > SEARCH_TELEMETRY_MS_THRESHOLD ||
        searchCountRef.current % SEARCH_TELEMETRY_SAMPLE === 0
      ) {
        fireTelemetry("search_performed", {
          query_len: queryLen,
          results,
          duration_ms: duration,
        });
      }
    },
    [],
  );

  // Query the Name Index for a Tab, cancel-on-newer via a per-Tab seq. The empty
  // query short-circuits (the reducer already showed idle). A slow timer marks the
  // query slow if it is still in flight past the threshold; the query itself is
  // async and never blocks typing (§10).
  const runSearch = useCallback(
    (tabId: TabId, query: string): void => {
      const seq = (searchSeqRef.current.get(tabId) ?? 0) + 1;
      searchSeqRef.current.set(tabId, seq);
      clearSlowTimer(tabId);
      if (query.trim() === "") {
        return;
      }
      const started = performance.now();
      slowTimerRef.current.set(
        tabId,
        setTimeout(() => {
          slowTimerRef.current.delete(tabId);
          if (searchSeqRef.current.get(tabId) !== seq) {
            return;
          }
          dispatch({
            type: "search",
            tabId,
            action: { type: "resultsSlow", query },
          });
        }, SEARCH_SLOW_MS),
      );
      void searchNameIndex(query, SEARCH_LIMIT).match(
        (response) => {
          if (searchSeqRef.current.get(tabId) !== seq) {
            return;
          }
          clearSlowTimer(tabId);
          fireSearchTelemetry(
            query.length,
            response.hits.length,
            performance.now() - started,
          );
          dispatch({
            type: "search",
            tabId,
            action: { type: "resultsLanded", query, hits: response.hits },
          });
        },
        (error) => {
          if (searchSeqRef.current.get(tabId) !== seq) {
            return;
          }
          clearSlowTimer(tabId);
          reportShellError(error);
          dispatch({
            type: "search",
            tabId,
            action: { type: "resultsFailed", query },
          });
        },
      );
    },
    [clearSlowTimer, fireSearchTelemetry],
  );

  // Focus and select the Navigation Input on the next frame (after the state
  // change has painted), so activation always shows the retained query selected.
  const focusInput = useCallback((): void => {
    requestAnimationFrame(() => {
      const element = inputRef.current;
      if (element !== null) {
        element.focus();
        element.select();
      }
    });
  }, []);

  const activateSearch = useCallback((): void => {
    const current = stateRef.current;
    const active = tabById(current, current.activeId);
    if (active !== undefined && active.search.mode === "search") {
      // Already active: just re-focus and re-select the retained query.
      focusInput();
      return;
    }
    dispatch({
      type: "search",
      tabId: current.activeId,
      action: { type: "activate" },
    });
    focusInput();
    // Re-run the retained query so its Search Results reappear on activation.
    if (active !== undefined && active.search.query.trim() !== "") {
      runSearch(current.activeId, active.search.query);
    }
  }, [focusInput, runSearch]);

  const deactivateSearch = useCallback((): void => {
    const activeId = stateRef.current.activeId;
    clearSlowTimer(activeId);
    dispatch({ type: "search", tabId: activeId, action: { type: "deactivate" } });
    inputRef.current?.blur();
  }, [clearSlowTimer]);

  const changeQuery = useCallback(
    (text: string): void => {
      const activeId = stateRef.current.activeId;
      dispatch({
        type: "search",
        tabId: activeId,
        action: { type: "queryChanged", query: text },
      });
      runSearch(activeId, text);
    },
    [runSearch],
  );

  const focusResultDelta = useCallback((delta: number): void => {
    dispatch({
      type: "search",
      tabId: stateRef.current.activeId,
      action: { type: "focusDelta", delta },
    });
  }, []);

  const activateFocused = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined || active.browse.load.status !== "ready") {
      return;
    }
    const focused = active.browse.load.items[active.browse.focusedIndex];
    if (focused !== undefined) {
      activateItem(focused);
    }
  }, [activateItem]);

  const goBack = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    const entry = active?.browse.history[active.browse.history.length - 1];
    if (active !== undefined && entry !== undefined) {
      navigate(active.id, entry.location, "back");
    }
  }, [navigate]);

  const goForward = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    const entry = active?.browse.future[active.browse.future.length - 1];
    if (active !== undefined && entry !== undefined) {
      navigate(active.id, entry.location, "forward");
    }
  }, [navigate]);

  const activateTab = useCallback((id: TabId): void => {
    const current = stateRef.current;
    if (id === current.activeId) {
      return;
    }
    const target = tabById(current, id);
    if (target === undefined) {
      return;
    }
    dispatch({
      type: "activate",
      tabId: id,
      outgoingScrollTop: scrollTopRef.current,
      nowMs: Date.now(),
    });
    // The live scroll ref now tracks the incoming Tab's saved offset.
    scrollTopRef.current = target.browse.scrollTop;
  }, []);

  // Open or reuse a Temporary Tab at `target` for a Reveal (§4): reuse a Temporary
  // Tab already exactly at the target Location, otherwise create one. Either way
  // the Location entered is recorded (the loads here are `replace`, so recording
  // is forced).
  const openOrReuseTemporary = useCallback(
    (target: string, focusPath: string | undefined, originId: TabId): void => {
      const current = stateRef.current;
      const existing = current.tabs.find(
        (tab) =>
          tab.kind === "temporary" &&
          tab.browse.location.kind === "directory" &&
          tab.browse.location.path === target,
      );
      if (existing !== undefined) {
        if (existing.id !== current.activeId) {
          activateTab(existing.id);
        }
        if (focusPath !== undefined) {
          // Re-focus the revealed file without pushing a bogus history step.
          navigate(existing.id, directoryLocation(target), "replace", focusPath, true);
        } else {
          void recordVisit(target, "entered_location").match(
            () => undefined,
            reportShellError,
          );
        }
        return;
      }
      const nowMs = Date.now();
      const id = freshTabId();
      const tab = makeTemporaryTab({ id, nowMs, originatorId: originId });
      // The originator is a Pinned Tab here, so insert after the Pinned group.
      const index = pinnedCount(current.tabs);
      dispatch({
        type: "create",
        tab,
        index,
        outgoingScrollTop: scrollTopRef.current,
        nowMs,
      });
      scrollTopRef.current = 0;
      navigate(id, directoryLocation(target), "replace", focusPath, true);
      fireTelemetry("tab_created", { kind: "temporary" });
    },
    [activateTab, navigate],
  );

  // Reveal a Search Result (CONTEXT.md): a file opens its containing Location and
  // focuses it; a directory is entered. Reveal ends Search Mode and routes into the
  // right Tab per §4. Never the Item's primary action.
  const revealHit = useCallback(
    (hit: SearchHit): void => {
      const current = stateRef.current;
      const originId = current.activeId;
      const origin = tabById(current, originId);
      if (origin === undefined) {
        return;
      }
      const isDir = hit.isDirectory;
      const target = isDir ? hit.path : parentPath(hit.path);
      const focusPath = isDir ? undefined : hit.path;

      // Reveal transitions the origin Tab out of Search Mode; the query is retained.
      clearSlowTimer(originId);
      dispatch({
        type: "search",
        tabId: originId,
        action: { type: "deactivate" },
      });
      inputRef.current?.blur();

      const anchor = mostSpecificAnchor(current.tabs, target);
      // A file at/below an Anchor (→ its containing Location) or the Anchor itself
      // activates that Pinned Tab — the primary routing rule, whatever the origin
      // Tab (§4).
      if (anchor !== undefined && (!isDir || anchor.anchorPath === target)) {
        if (anchor.id !== originId) {
          activateTab(anchor.id);
        }
        navigate(anchor.id, directoryLocation(target), "enter", focusPath);
      } else if (origin.kind === "temporary") {
        // Otherwise, a Search begun in a Temporary Tab reuses it (§4).
        navigate(originId, directoryLocation(target), "enter", focusPath);
      } else {
        // A directory below an Anchor, or any result outside all Anchors, when
        // Search began in a Pinned Tab → a Temporary Tab (§4).
        openOrReuseTemporary(target, focusPath, originId);
      }
      fireTelemetry("reveal_performed", { kind: isDir ? "directory" : "file" });
    },
    [activateTab, navigate, openOrReuseTemporary, clearSlowTimer],
  );

  const revealResultAt = useCallback(
    (index: number): void => {
      const active = tabById(stateRef.current, stateRef.current.activeId);
      if (active === undefined || active.search.mode !== "search") {
        return;
      }
      const hit = displayedHits(active.search.results)[index];
      if (hit !== undefined) {
        revealHit(hit);
      }
    },
    [revealHit],
  );

  const revealFocused = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined || active.search.mode !== "search") {
      return;
    }
    revealResultAt(active.search.focusedIndex);
  }, [revealResultAt]);

  // A click on the active Pinned Tab that is away from home returns it to its
  // Anchor (§4); any other click activates the Tab.
  const clickTab = useCallback(
    (id: TabId): void => {
      const current = stateRef.current;
      const tab = tabById(current, id);
      if (tab === undefined) {
        return;
      }
      if (id === current.activeId) {
        if (tab.kind === "pinned" && isOnExcursion(tab)) {
          navigate(id, directoryLocation(tab.anchorPath), "reset");
        }
        return;
      }
      activateTab(id);
    },
    [activateTab, navigate],
  );

  const newTemporaryTab = useCallback((): void => {
    const current = stateRef.current;
    const nowMs = Date.now();
    const id = freshTabId();
    const tab = makeTemporaryTab({ id, nowMs, originatorId: current.activeId });
    // Insert beside the originator, always after the Pinned group (§4).
    const activeIndex = current.tabs.findIndex((t) => t.id === current.activeId);
    const activeTab = current.tabs[activeIndex];
    const index =
      activeTab?.kind === "temporary"
        ? activeIndex + 1
        : pinnedCount(current.tabs);
    dispatch({
      type: "create",
      tab,
      index,
      outgoingScrollTop: scrollTopRef.current,
      nowMs,
    });
    scrollTopRef.current = 0;
    navigate(id, defaultEntryPoint(), "replace");
    // A new Temporary Tab starts with the Navigation Input active (§4, point 8).
    dispatch({ type: "search", tabId: id, action: { type: "activate" } });
    focusInput();
    fireTelemetry("tab_created", { kind: "temporary" });
  }, [navigate, focusInput]);

  const removeTab = useCallback((id: TabId): void => {
    const current = stateRef.current;
    const tab = tabById(current, id);
    if (tab === undefined) {
      return;
    }
    const nowMs = Date.now();
    const replacement = makeTemporaryTab({
      id: freshTabId(),
      nowMs,
      originatorId: null,
    });
    dispatch({
      type: "remove",
      tabId: id,
      replacement,
      outgoingScrollTop: scrollTopRef.current,
      nowMs,
    });
    // Closing the last Tab installs the clean replacement, which needs a listing.
    if (current.tabs.length === 1) {
      scrollTopRef.current = 0;
      navigate(replacement.id, defaultEntryPoint(), "replace");
    }
    fireTelemetry("tab_closed", { kind: tab.kind });
    seqRef.current.delete(id);
    searchSeqRef.current.delete(id);
    recentsMetaRef.current.delete(id);
    clearSlowTimer(id);
  }, [navigate, clearSlowTimer]);

  const pinTab = useCallback((id: TabId): void => {
    const current = stateRef.current;
    const tab = tabById(current, id);
    if (
      tab === undefined ||
      tab.kind !== "temporary" ||
      tab.browse.load.status !== "ready" ||
      // Only a directory can be an Anchor; Recents is not pinnable in v1.
      tab.browse.location.kind !== "directory"
    ) {
      return;
    }
    // Pinning a Location an Anchor already holds is a silent no-op (§4).
    const anchorPath = tab.browse.location.path;
    if (
      current.tabs.some(
        (t) => t.kind === "pinned" && t.anchorPath === anchorPath,
      )
    ) {
      return;
    }
    dispatch({ type: "pin", tabId: id });
    fireTelemetry("tab_pinned", {});
  }, []);

  const unpinTab = useCallback((id: TabId): void => {
    const tab = tabById(stateRef.current, id);
    if (tab === undefined || tab.kind !== "pinned") {
      return;
    }
    dispatch({ type: "unpin", tabId: id, nowMs: Date.now() });
    fireTelemetry("tab_unpinned", {});
  }, []);

  const renameTab = useCallback((id: TabId, name: string): void => {
    dispatch({ type: "rename", tabId: id, name });
  }, []);

  const copyLocation = useCallback((id: TabId): void => {
    const tab = tabById(stateRef.current, id);
    // Only a directory has a path to copy; Recents is a collection, not a Location path.
    if (
      tab === undefined ||
      tab.browse.location.kind !== "directory" ||
      tab.browse.location.path === ""
    ) {
      return;
    }
    void copyToClipboard(tab.browse.location.path).match(
      () => undefined,
      reportShellError,
    );
  }, []);

  const reorderTab = useCallback((id: TabId, targetIndex: number): void => {
    dispatch({ type: "reorder", tabId: id, targetIndex });
  }, []);

  // A drop into the other group pins or unpins; a drop within a group reorders.
  const dropOnGroup = useCallback(
    (id: TabId, group: TabGroup, targetIndex: number): void => {
      const tab = tabById(stateRef.current, id);
      if (tab === undefined) {
        return;
      }
      if (group === "pinned" && tab.kind === "temporary") {
        pinTab(id);
        return;
      }
      if (group === "temporary" && tab.kind === "pinned") {
        unpinTab(id);
        return;
      }
      reorderTab(id, targetIndex);
    },
    [pinTab, unpinTab, reorderTab],
  );

  // Whenever the active Tab changes, revalidate its cached listing (or retry a
  // failed one) so switching paints instantly, then refreshes in place (§10, §11).
  useEffect(() => {
    const active = tabById(stateRef.current, state.activeId);
    if (active === undefined) {
      return;
    }
    const load = active.browse.load;
    if (load.status === "ready") {
      revalidate(active.id, active.browse.location);
    } else if (load.status === "error" || load.status === "unavailable") {
      // A degraded Recents view (or a failed listing) fully re-pulls on activation, so a
      // recovered Spotlight replaces the explanatory line (revalidate only updates a
      // view that is already ready).
      navigate(active.id, active.browse.location, "replace");
    }
  }, [state.activeId, navigate, revalidate]);

  // One-time hydration: resolve home, load Pinned Tabs, and point the opening
  // Tabs at their Locations. Guarded against StrictMode's double invocation.
  const initedRef = useRef(false);
  const hydratedRef = useRef(false);
  useEffect(() => {
    if (initedRef.current) {
      return;
    }
    initedRef.current = true;
    void (async () => {
      const persisted = (await loadPinnedTabs()).match(
        (value) => value,
        (error) => {
          reportShellError(error);
          return [];
        },
      );
      if (persisted.length === 0) {
        // Keep the initial Temporary Tab; send it to the Default Entry Point (Recents,
        // §4/§7) and start with the Navigation Input active (§4, fresh Temporary Tab).
        const activeId = stateRef.current.activeId;
        navigate(activeId, defaultEntryPoint(), "replace");
        dispatch({ type: "search", tabId: activeId, action: { type: "activate" } });
        focusInput();
        hydratedRef.current = true;
        return;
      }
      const pinnedTabs: PinnedTab[] = persisted.map((entry) => ({
        kind: "pinned",
        id: freshTabId(),
        anchorPath: entry.anchorPath,
        customName: entry.customName,
        browse: initialBrowseState,
        search: initialSearchState,
      }));
      const first = pinnedTabs[0];
      if (first === undefined) {
        hydratedRef.current = true;
        return;
      }
      dispatch({
        type: "hydrate",
        state: { tabs: pinnedTabs, activeId: first.id },
      });
      hydratedRef.current = true;
      for (const pinnedTab of pinnedTabs) {
        navigate(pinnedTab.id, directoryLocation(pinnedTab.anchorPath), "replace");
      }
    })();
  }, [navigate, focusInput]);

  // Persist Pinned Tabs (Anchor, custom name, order) promptly on every change.
  const pinnedSignature = useMemo(
    () =>
      JSON.stringify(
        state.tabs
          .filter(isPinned)
          .map((tab) => [tab.anchorPath, tab.customName]),
      ),
    [state.tabs],
  );
  useEffect(() => {
    // Never write before hydration or the empty opening state would clobber disk.
    if (!hydratedRef.current) {
      return;
    }
    const pinned = stateRef.current.tabs.filter(isPinned).map((tab) => ({
      anchorPath: tab.anchorPath,
      customName: tab.customName,
    }));
    void savePinnedTabs(pinned).match(() => undefined, reportShellError);
  }, [pinnedSignature]);

  // On show (§9): stamp the active Tab, reset stale Excursions after 5 background
  // minutes, and drop expired Temporary Tabs — never while the window was visible.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void subscribeWindowShown((payload) => {
      const current = stateRef.current;
      const nowMs = Date.now();
      dispatch({ type: "touchActive", nowMs });
      if (payload.hiddenMs !== null && payload.hiddenMs >= BACKGROUND_RESET_MS) {
        const resets: PinnedTab[] = excursionsToReset(current.tabs);
        if (resets.length > 0) {
          for (const pinnedTab of resets) {
            navigate(pinnedTab.id, directoryLocation(pinnedTab.anchorPath), "reset");
          }
          fireTelemetry("excursions_reset", { count: resets.length });
        }
      }
      const expired = expiredTemporaryIds(current.tabs, current.activeId, nowMs);
      if (expired.length > 0) {
        dispatch({ type: "removeExpired", ids: expired });
        for (const id of expired) {
          fireTelemetry("tab_closed", { kind: "temporary" });
          seqRef.current.delete(id);
          searchSeqRef.current.delete(id);
          recentsMetaRef.current.delete(id);
          clearSlowTimer(id);
        }
      }
    }).match((fn) => {
      unlisten = fn;
    }, reportShellError);
    return () => {
      if (unlisten !== null) {
        unlisten();
      }
    };
  }, [navigate, clearSlowTimer]);

  // A background Recents refresh landed (§7): re-pull the active Tab if it is showing
  // Recents so the fresh list replaces the cached one in place. Other Recents Tabs
  // re-pull on their next activation via the revalidate effect above.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void subscribeRecentsUpdated(() => {
      const current = stateRef.current;
      const active = tabById(current, current.activeId);
      if (active === undefined || active.browse.location.kind !== "recents") {
        return;
      }
      // A ready view updates in place; a degraded one fully re-pulls so a recovered
      // Spotlight replaces the explanatory line (revalidate needs a ready view).
      if (active.browse.load.status === "ready") {
        revalidate(active.id, recentsLocation);
      } else {
        navigate(active.id, recentsLocation, "replace");
      }
    }).match((fn) => {
      unlisten = fn;
    }, reportShellError);
    return () => {
      if (unlisten !== null) {
        unlisten();
      }
    };
  }, [navigate, revalidate]);

  // Combined keyboard contract: Tab shortcuts plus the Browse-mode keys, in the
  // capture phase so Escape can preventDefault before shell.ts's bubble hide.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (event.defaultPrevented || isEditableTarget(event.target)) {
        return;
      }

      if (event.metaKey) {
        const digit = Number.parseInt(event.key, 10);
        if (!Number.isNaN(digit) && digit >= 1 && digit <= 8) {
          event.preventDefault();
          const tab = stateRef.current.tabs[digit - 1];
          if (tab !== undefined) {
            activateTab(tab.id);
          }
          return;
        }
        switch (event.key) {
          case "9": {
            event.preventDefault();
            const tabs = stateRef.current.tabs;
            const last = tabs[tabs.length - 1];
            if (last !== undefined) {
              activateTab(last.id);
            }
            return;
          }
          case "[":
            event.preventDefault();
            goBack();
            return;
          case "]":
            event.preventDefault();
            goForward();
            return;
          case "l":
          case "L":
            // Cmd+L activates Search Mode and selects the retained query (§5).
            event.preventDefault();
            activateSearch();
            return;
          case "t":
          case "T":
            event.preventDefault();
            newTemporaryTab();
            return;
          case "w":
          case "W": {
            event.preventDefault();
            const active = tabById(stateRef.current, stateRef.current.activeId);
            if (active === undefined) {
              return;
            }
            if (active.kind === "pinned") {
              void requestHide("pinned-tab-close").match(
                () => undefined,
                reportShellError,
              );
            } else {
              removeTab(active.id);
            }
            return;
          }
        }
        return;
      }

      if (event.ctrlKey) {
        if (event.key === "Tab") {
          event.preventDefault();
          const tabs = stateRef.current.tabs;
          const count = tabs.length;
          const index = tabs.findIndex(
            (tab) => tab.id === stateRef.current.activeId,
          );
          const step = event.shiftKey ? -1 : 1;
          const next = tabs[(index + step + count) % count];
          if (next !== undefined) {
            activateTab(next.id);
          }
          return;
        }
        if (event.key === "j" || event.key === "J") {
          event.preventDefault();
          focusDelta(1, event.shiftKey);
          return;
        }
        if (event.key === "k" || event.key === "K") {
          event.preventDefault();
          focusDelta(-1, event.shiftKey);
          return;
        }
        return;
      }

      // The physical Slash key activates Search Mode, layout-independent (§5).
      // `event.code` is the physical key, so RU and EN layouts both hit it.
      if (event.code === "Slash" && !event.altKey) {
        event.preventDefault();
        activateSearch();
        return;
      }

      switch (event.key) {
        case "ArrowDown":
          event.preventDefault();
          focusDelta(1, event.shiftKey);
          break;
        case "ArrowUp":
          event.preventDefault();
          focusDelta(-1, event.shiftKey);
          break;
        case "ArrowRight":
        case "Enter":
          event.preventDefault();
          activateFocused();
          break;
        case "ArrowLeft":
          event.preventDefault();
          goBack();
          break;
        case "Escape": {
          const active = tabById(stateRef.current, stateRef.current.activeId);
          if (active !== undefined && hasSelectionBeyondFocus(active.browse)) {
            event.preventDefault();
            dispatch({
              type: "browse",
              tabId: active.id,
              action: { type: "clearSelection" },
            });
          }
          break;
        }
      }
    }
    document.addEventListener("keydown", onKeyDown, { capture: true });
    return () => {
      document.removeEventListener("keydown", onKeyDown, { capture: true });
    };
  }, [
    activateTab,
    activateFocused,
    activateSearch,
    focusDelta,
    goBack,
    goForward,
    newTemporaryTab,
    removeTab,
  ]);

  const activeTab = useMemo(() => {
    const found =
      state.tabs.find((tab) => tab.id === state.activeId) ?? state.tabs[0];
    if (found === undefined) {
      throw new Error("Tabs invariant broken: the list is empty");
    }
    return found;
  }, [state]);

  return {
    state,
    activeTab,
    activeBrowse: activeTab.browse,
    activeSearch: activeTab.search,
    select,
    activateItem,
    onScrollTop,
    loadMoreRecents,
    setInputEl,
    activateSearch,
    deactivateSearch,
    changeQuery,
    focusResultDelta,
    revealFocused,
    revealResultAt,
    clickTab,
    newTemporaryTab,
    removeTab,
    pinTab,
    unpinTab,
    renameTab,
    copyLocation,
    reorderTab,
    dropOnGroup,
  };
}
