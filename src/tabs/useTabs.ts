import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { openPath, primaryActionFor } from "../browse/actions";
import { afterEffectFor, type AfterActionId } from "../operations/afterAction";
import {
  createFolder,
  deleteItemsPermanently,
  opErrorMessage,
  openInApp,
  pasteCopy,
  pasteMove,
  renameItem,
  resolveInstalledBundle,
  revealInFinder,
  subscribeOperationFinished,
  subscribeOperationProgress,
  trashItems,
  type JobId,
} from "../operations/ipc";
import {
  EDITOR_BUNDLE_IDS,
  loadAppSettings,
  saveAppSettings,
  TERMINAL_BUNDLE_IDS,
  type AppSettings,
  type Slot,
} from "../operations/settings";
import {
  initialOpsState,
  opsReducer,
  type MenuItem,
  type MenuVia,
  type OpsState,
} from "../operations/state";
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
  readClipboardText,
  recordTelemetry,
  reportShellError,
  requestHide,
  requestQuit,
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
  folderName,
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
import { strings } from "../strings";

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
  // Operations (§8): clipboard, batch jobs, the Action Menu, inline rename, the delete
  // confirm, and the Status Strip problem list. State is read for rendering; the actions
  // the components need directly are exposed alongside it.
  ops: OpsState;
  menuItems: MenuItem[];
  // Open the Action Menu from a row (`…` control or context click), selecting the row
  // first when it is not already in the Selected Items (SPEC §5: one menu, three ways).
  openRowMenu: (index: number, via: MenuVia, x: number, y: number) => void;
  focusMenuItem: (index: number) => void;
  runMenuItem: (item: MenuItem) => void;
  closeActionMenu: () => void;
  // Inline rename (§8): commit via the engine, or cancel; the collision message shows
  // inline in the row (`ops.rename.error`).
  commitRename: (newName: string) => void;
  cancelRename: () => void;
  // The always-on Delete Permanently confirm (§8).
  confirmDelete: () => void;
  cancelDelete: () => void;
  // Dismiss one Status Strip problem (§8, §13).
  dismissProblem: (id: number) => void;
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

// The paths of the Selected Items of a browse view, in row order; empty unless the view is
// ready (SPEC §8: operations act on Selected Items).
function selectedPaths(browse: BrowseState): string[] {
  if (browse.load.status !== "ready") {
    return [];
  }
  const items = browse.load.items;
  const paths: string[] = [];
  for (const index of browse.selected) {
    const item = items[index];
    if (item !== undefined) {
      paths.push(item.path);
    }
  }
  return paths;
}

// The single Focused Item of a browse view (anchors Rename, Reveal, Open in Terminal/Editor,
// Open in New Tab), or undefined when the view is not ready or empty.
function focusedItemOf(browse: BrowseState): Item | undefined {
  if (browse.load.status !== "ready") {
    return undefined;
  }
  return browse.load.items[browse.focusedIndex];
}

// The directory path a paste / New Folder targets, or null when there is none — a Recents
// Tab has no target directory, so Paste and New Folder are disabled there (SPEC §8, point 1).
function currentDirPath(browse: BrowseState): string | null {
  return browse.location.kind === "directory" && browse.location.path !== ""
    ? browse.location.path
    : null;
}

// The next enabled Action Menu row in `delta` direction, skipping disabled rows and
// wrapping; returns `from` when nothing else is enabled (SPEC §5 keyboard).
function nextEnabledIndex(
  items: readonly MenuItem[],
  from: number,
  delta: number,
): number {
  const count = items.length;
  if (count === 0) {
    return from;
  }
  let index = from;
  for (let step = 0; step < count; step += 1) {
    index = (index + delta + count) % count;
    if (items[index]?.disabled === false) {
      return index;
    }
  }
  return from;
}

export function useTabs(): Tabs {
  // The opening Tab is stamped at 0; the first show (or navigation) re-stamps it,
  // and it is the active Tab so lifetime expiry never touches it meanwhile.
  const [state, dispatch] = useReducer(tabsReducer, 0, createInitialTabsState);

  // Operations state (§8): clipboard, batch jobs, Status Strip problems, the Action Menu,
  // inline rename, and the delete confirm. Its own reducer so the Tab machinery stays clean.
  const [ops, opsDispatch] = useReducer(opsReducer, initialOpsState);

  // Latest state and live active-Tab scroll for event handlers that must not
  // close over a stale snapshot.
  const stateRef = useRef(state);
  useEffect(() => {
    stateRef.current = state;
  }, [state]);
  const opsRef = useRef(ops);
  useEffect(() => {
    opsRef.current = ops;
  }, [ops]);
  const scrollTopRef = useRef(0);

  // The two auto-seeded application slots (§8): loaded once, updated in place when a slot
  // is first resolved so a later Open in Terminal/Editor never re-probes.
  const appSettingsRef = useRef<AppSettings>({
    terminalBundleId: null,
    editorBundleId: null,
  });
  useEffect(() => {
    void loadAppSettings().match((loaded) => {
      appSettingsRef.current = loaded;
    }, reportShellError);
  }, []);

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

  // The single After Action policy (§12): consult the defaults table and hide the window
  // when the action calls for it. Settings replaces the fixed table later (#30).
  const afterAction = useCallback((action: AfterActionId): void => {
    const effect = afterEffectFor(action);
    fireTelemetry("after_action", { action, effect });
    if (effect === "hide") {
      void requestHide("after-action").match(() => undefined, reportShellError);
    }
  }, []);

  const openFile = useCallback(
    (path: string): void => {
      void openPath(path).match(() => {
        fireTelemetry("file_opened", { path });
        void recordVisit(path, "opened_file").match(
          () => undefined,
          reportShellError,
        );
        // Opening a file hides the window (§12 After Action default).
        afterAction("open_file");
      }, reportShellError);
    },
    [afterAction],
  );

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

  // ---- Operations (§8) --------------------------------------------------------------

  // Cmd+C / Copy File: put the Selected Items on the in-app clipboard as file references
  // (Finder model) and mirror the paths to the system clipboard as newline-joined text for
  // interop (§8, point 1). Copy File hides the window (§12 After Action).
  const copySelection = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const paths = selectedPaths(active.browse);
    if (paths.length === 0) {
      return;
    }
    opsDispatch({ type: "setClipboard", paths });
    void copyToClipboard(paths.join("\n")).match(() => undefined, reportShellError);
    fireTelemetry("clipboard_copied", { count: paths.length });
    afterAction("copy_file");
  }, [afterAction]);

  // Copy Path: textual only (§8) — the path(s) to the system clipboard, newline-separated.
  // Does not touch the in-app file-reference clipboard. Copy Path hides the window (§12).
  const copyPath = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const paths = selectedPaths(active.browse);
    if (paths.length === 0) {
      return;
    }
    void copyToClipboard(paths.join("\n")).match(() => {
      afterAction("copy_path");
    }, reportShellError);
  }, [afterAction]);

  // Cmd+V / Paste: paste-copy the clipboard file references into the current Location; a
  // same-Location paste duplicates (the engine suffixes). No-op without a clipboard or a
  // target directory (a Recents Tab has neither). Keep the window open (§12).
  const pasteIntoLocation = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const clip = opsRef.current.clipboard;
    const dir = currentDirPath(active.browse);
    if (clip === null || clip.length === 0 || dir === null) {
      return;
    }
    void pasteCopy(clip, dir).match(
      (jobId) => {
        opsDispatch({
          type: "jobStarted",
          jobId,
          label: strings.operations.status.copying,
          total: clip.length,
        });
      },
      (error) => {
        opsDispatch({ type: "pushProblem", path: dir, cause: opErrorMessage(error) });
      },
    );
    afterAction("paste");
  }, [afterAction]);

  // Cmd+Opt+V / Move Here: paste-move the clipboard into the current Location and clear the
  // clipboard (§8, point 1). Keep the window open (§12).
  const movePasteIntoLocation = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const clip = opsRef.current.clipboard;
    const dir = currentDirPath(active.browse);
    if (clip === null || clip.length === 0 || dir === null) {
      return;
    }
    void pasteMove(clip, dir).match(
      (jobId) => {
        opsDispatch({
          type: "jobStarted",
          jobId,
          label: strings.operations.status.moving,
          total: clip.length,
        });
        opsDispatch({ type: "setClipboard", paths: null });
      },
      (error) => {
        opsDispatch({ type: "pushProblem", path: dir, cause: opErrorMessage(error) });
      },
    );
    afterAction("move_paste");
  }, [afterAction]);

  // Cmd+Delete / Move to Trash: no confirmation, ever (§8). Rows disappear only after the
  // finished event refreshes the listing. Keep the window open (§12).
  const trashSelection = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const paths = selectedPaths(active.browse);
    if (paths.length === 0) {
      return;
    }
    void trashItems(paths).match(
      (jobId) => {
        opsDispatch({
          type: "jobStarted",
          jobId,
          label: strings.operations.status.trashing,
          total: paths.length,
        });
      },
      (error) => {
        opsDispatch({
          type: "pushProblem",
          path: paths[0] ?? "",
          cause: opErrorMessage(error),
        });
      },
    );
    afterAction("trash");
  }, [afterAction]);

  // Opt+Cmd+Delete / Delete Permanently: always opens the confirm (§8, no "don't ask
  // again"); the delete itself runs only on confirm.
  const requestDeleteSelection = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const paths = selectedPaths(active.browse);
    if (paths.length === 0) {
      return;
    }
    opsDispatch({ type: "openConfirm", paths });
  }, []);

  const confirmDelete = useCallback((): void => {
    const confirm = opsRef.current.confirm;
    if (confirm === null) {
      return;
    }
    opsDispatch({ type: "closeConfirm" });
    void deleteItemsPermanently(confirm.paths).match(
      (jobId) => {
        opsDispatch({
          type: "jobStarted",
          jobId,
          label: strings.operations.status.deleting,
          total: confirm.paths.length,
        });
      },
      (error) => {
        opsDispatch({
          type: "pushProblem",
          path: confirm.paths[0] ?? "",
          cause: opErrorMessage(error),
        });
      },
    );
    afterAction("delete_permanently");
  }, [afterAction]);

  const cancelDelete = useCallback((): void => {
    opsDispatch({ type: "closeConfirm" });
  }, []);

  // Inline rename (§8): seed the field with the Focused Item's current name.
  const startRename = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const item = focusedItemOf(active.browse);
    if (item === undefined) {
      return;
    }
    opsDispatch({ type: "startRename", path: item.path, initialName: item.name });
  }, []);

  const cancelRename = useCallback((): void => {
    opsDispatch({ type: "cancelRename" });
  }, []);

  // Commit an inline rename (§8): a collision surfaces as an inline row message and keeps
  // the field open; any other failure closes the field and lands in the Status Strip. On
  // success the listing re-lists and focuses the renamed Item.
  const commitRename = useCallback(
    (newName: string): void => {
      const rename = opsRef.current.rename;
      if (rename === null) {
        return;
      }
      const trimmed = newName.trim();
      if (trimmed === "" || trimmed === rename.initialName) {
        opsDispatch({ type: "cancelRename" });
        return;
      }
      const activeId = stateRef.current.activeId;
      const location = tabById(stateRef.current, activeId)?.browse.location;
      void renameItem(rename.path, trimmed).match(
        (newPath) => {
          opsDispatch({ type: "cancelRename" });
          if (location !== undefined) {
            navigate(activeId, location, "replace", newPath);
          }
        },
        (error) => {
          if (error.code === "name-collision") {
            opsDispatch({ type: "renameError", error: opErrorMessage(error) });
          } else {
            opsDispatch({ type: "cancelRename" });
            opsDispatch({
              type: "pushProblem",
              path: rename.path,
              cause: opErrorMessage(error),
            });
          }
        },
      );
    },
    [navigate],
  );

  // New Folder (§8): create in the current Location, then enter inline rename on the new
  // row. No-op in a Recents Tab (no target directory). Keep the window open (§12).
  const newFolder = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const dir = currentDirPath(active.browse);
    if (dir === null) {
      return;
    }
    const activeId = active.id;
    const location = active.browse.location;
    void createFolder(dir, strings.operations.newFolderName).match(
      (newPath) => {
        navigate(activeId, location, "replace", newPath);
        opsDispatch({
          type: "startRename",
          path: newPath,
          initialName: folderName(newPath),
        });
      },
      (error) => {
        opsDispatch({ type: "pushProblem", path: dir, cause: opErrorMessage(error) });
      },
    );
    afterAction("new_folder");
  }, [navigate, afterAction]);

  // Resolve a Terminal/Editor slot (§8): the persisted bundle id if seeded, otherwise the
  // first installed app in the priority list — persisted on first resolve. null means none
  // is installed, so the caller routes to a Status Strip problem (Settings UI is #30).
  const resolveSlot = useCallback(async (slot: Slot): Promise<string | null> => {
    const settings = appSettingsRef.current;
    const existing =
      slot === "terminal" ? settings.terminalBundleId : settings.editorBundleId;
    if (existing !== null) {
      return existing;
    }
    const ids = slot === "terminal" ? TERMINAL_BUNDLE_IDS : EDITOR_BUNDLE_IDS;
    const resolved = await resolveInstalledBundle([...ids]).match(
      (value) => value,
      (error) => {
        reportShellError(error);
        return null;
      },
    );
    if (resolved !== null) {
      const next: AppSettings =
        slot === "terminal"
          ? { ...settings, terminalBundleId: resolved }
          : { ...settings, editorBundleId: resolved };
      appSettingsRef.current = next;
      void saveAppSettings(next).match(() => undefined, reportShellError);
    }
    return resolved;
  }, []);

  // Open in Terminal (§8): a directory opens itself; a file opens its containing Location.
  const openInTerminal = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const item = focusedItemOf(active.browse);
    if (item === undefined) {
      return;
    }
    const target = item.isDirectory ? item.path : parentPath(item.path);
    void (async () => {
      const bundle = await resolveSlot("terminal");
      if (bundle === null) {
        opsDispatch({
          type: "pushProblem",
          path: target,
          cause: strings.operations.status.noTerminal,
        });
        return;
      }
      void openInApp(target, bundle).match(
        () => {
          afterAction("open_terminal");
        },
        (error) => {
          opsDispatch({
            type: "pushProblem",
            path: target,
            cause: opErrorMessage(error),
          });
        },
      );
    })();
  }, [afterAction, resolveSlot]);

  // Open in Editor (§8): a file opens as a file; a directory opens as a project. `open -b`
  // hands the path to the editor, which treats a directory argument as a project root.
  const openInEditor = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const item = focusedItemOf(active.browse);
    if (item === undefined) {
      return;
    }
    const target = item.path;
    void (async () => {
      const bundle = await resolveSlot("editor");
      if (bundle === null) {
        opsDispatch({
          type: "pushProblem",
          path: target,
          cause: strings.operations.status.noEditor,
        });
        return;
      }
      void openInApp(target, bundle).match(
        () => {
          afterAction("open_editor");
        },
        (error) => {
          opsDispatch({
            type: "pushProblem",
            path: target,
            cause: opErrorMessage(error),
          });
        },
      );
    })();
  }, [afterAction, resolveSlot]);

  const revealSelection = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const item = focusedItemOf(active.browse);
    if (item === undefined) {
      return;
    }
    void revealInFinder(item.path).match(
      () => {
        afterAction("reveal");
      },
      (error) => {
        opsDispatch({
          type: "pushProblem",
          path: item.path,
          cause: opErrorMessage(error),
        });
      },
    );
  }, [afterAction]);

  // Menu Open: run the Focused Item's primary action (a file opens and hides; a directory
  // is entered). Reuses the double-click / Enter path so behavior stays identical.
  const openSelected = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    const item = focusedItemOf(active.browse);
    if (item !== undefined) {
      activateItem(item);
    }
  }, [activateItem]);

  // Open in New Tab (§8, §4): always creates a Temporary Tab — a directory opens at itself,
  // a file at its containing Location with the file focused. Keep the window open (§12).
  const openInNewTab = useCallback((): void => {
    const current = stateRef.current;
    const active = tabById(current, current.activeId);
    if (active === undefined) {
      return;
    }
    const item = focusedItemOf(active.browse);
    if (item === undefined) {
      return;
    }
    const target = item.isDirectory ? item.path : parentPath(item.path);
    const focusPath = item.isDirectory ? undefined : item.path;
    const nowMs = Date.now();
    const id = freshTabId();
    const tab = makeTemporaryTab({ id, nowMs, originatorId: current.activeId });
    const activeIndex = current.tabs.findIndex((t) => t.id === current.activeId);
    const originator = current.tabs[activeIndex];
    const index =
      originator?.kind === "temporary" ? activeIndex + 1 : pinnedCount(current.tabs);
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
    afterAction("open_in_new_tab");
  }, [navigate, afterAction]);

  // App-scope Refresh: re-list the active Tab in place. Keep the window open (§12).
  const refresh = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    if (active.browse.load.status === "ready") {
      revalidate(active.id, active.browse.location);
    } else {
      navigate(active.id, active.browse.location, "replace");
    }
    afterAction("navigation");
  }, [revalidate, navigate, afterAction]);

  // App-scope Paste Path (§5): drop the system clipboard text into the Navigation Input as
  // a Search Query (not a file paste).
  const pastePath = useCallback((): void => {
    void readClipboardText().match((text) => {
      activateSearch();
      changeQuery(text);
    }, reportShellError);
  }, [activateSearch, changeQuery]);

  const quit = useCallback((): void => {
    void requestQuit().match(() => undefined, reportShellError);
  }, []);

  // Open the Action Menu by keyboard (§5, Cmd+K): app scope with no Selected Items, item
  // scope otherwise. Position is derived at render (no anchor point).
  const openActionMenu = useCallback((via: MenuVia): void => {
    opsDispatch({ type: "openMenu", via, x: null, y: null, focusedIndex: 0 });
    fireTelemetry("action_menu_opened", { via });
  }, []);

  // Open the Action Menu from a row (§5, `…` control or context click), selecting the row
  // first unless it is already among the Selected Items (Finder behavior).
  const openRowMenu = useCallback(
    (index: number, via: MenuVia, x: number, y: number): void => {
      const active = tabById(stateRef.current, stateRef.current.activeId);
      if (active === undefined || active.browse.load.status !== "ready") {
        return;
      }
      if (!active.browse.selected.has(index)) {
        dispatch({
          type: "browse",
          tabId: active.id,
          action: { type: "select", index, mode: "plain" },
        });
      }
      opsDispatch({ type: "openMenu", via, x, y, focusedIndex: 0 });
      fireTelemetry("action_menu_opened", { via });
    },
    [],
  );

  const focusMenuItem = useCallback((index: number): void => {
    opsDispatch({ type: "menuFocus", index });
  }, []);

  const closeActionMenu = useCallback((): void => {
    opsDispatch({ type: "closeMenu" });
  }, []);

  const runMenuItem = useCallback((item: MenuItem): void => {
    opsDispatch({ type: "closeMenu" });
    if (!item.disabled) {
      item.run();
    }
  }, []);

  const dismissProblem = useCallback((id: number): void => {
    opsDispatch({ type: "dismissProblem", id });
  }, []);

  // The Action Menu's rows, derived from the live selection each render (§5): file actions
  // with Selected Items, application actions without.
  const menuItems = useMemo<MenuItem[]>(() => {
    const active = state.tabs.find((tab) => tab.id === state.activeId);
    const browse = active?.browse;
    const selCount = browse === undefined ? 0 : selectedPaths(browse).length;
    if (browse === undefined || selCount === 0) {
      // Application actions (§5): reachable via Cmd+K with an empty selection.
      const menu = strings.operations.menu;
      return [
        { id: "newTab", label: menu.newTab, disabled: false, run: newTemporaryTab },
        { id: "pastePath", label: menu.pastePath, disabled: false, run: pastePath },
        { id: "refresh", label: menu.refresh, disabled: false, run: refresh },
        { id: "settings", label: menu.settings, disabled: true, run: () => undefined },
        { id: "quit", label: menu.quit, disabled: false, run: quit },
      ];
    }
    const menu = strings.operations.menu;
    const isRecents = browse.location.kind === "recents";
    const canPaste =
      ops.clipboard !== null &&
      ops.clipboard.length > 0 &&
      currentDirPath(browse) !== null;
    const single = selCount === 1;
    return [
      { id: "open", label: menu.open, disabled: false, run: openSelected },
      // Quick Look is still absent in chunk B — a disabled placeholder (§9, ticket).
      { id: "quickLook", label: menu.quickLook, disabled: true, run: () => undefined },
      { id: "copyPath", label: menu.copyPath, disabled: false, run: copyPath },
      { id: "copyFile", label: menu.copyFile, disabled: false, run: copySelection },
      { id: "paste", label: menu.paste, disabled: !canPaste, run: pasteIntoLocation },
      {
        id: "movePaste",
        label: menu.movePaste,
        disabled: !canPaste,
        run: movePasteIntoLocation,
      },
      { id: "rename", label: menu.rename, disabled: !single, run: startRename },
      { id: "newFolder", label: menu.newFolder, disabled: isRecents, run: newFolder },
      { id: "trash", label: menu.trash, disabled: false, run: trashSelection },
      {
        id: "deletePermanently",
        label: menu.deletePermanently,
        disabled: false,
        run: requestDeleteSelection,
      },
      { id: "reveal", label: menu.reveal, disabled: false, run: revealSelection },
      {
        id: "openTerminal",
        label: menu.openTerminal,
        disabled: false,
        run: openInTerminal,
      },
      { id: "openEditor", label: menu.openEditor, disabled: false, run: openInEditor },
      {
        id: "openInNewTab",
        label: menu.openInNewTab,
        disabled: false,
        run: openInNewTab,
      },
    ];
  }, [
    state,
    ops.clipboard,
    newTemporaryTab,
    pastePath,
    refresh,
    quit,
    openSelected,
    copyPath,
    copySelection,
    pasteIntoLocation,
    movePasteIntoLocation,
    startRename,
    newFolder,
    trashSelection,
    requestDeleteSelection,
    revealSelection,
    openInTerminal,
    openInEditor,
    openInNewTab,
  ]);

  // The keyboard handler runs the focused row without re-deriving the menu; keep the latest
  // rows in a ref so it never closes over a stale list.
  const menuItemsRef = useRef<MenuItem[]>(menuItems);
  useEffect(() => {
    menuItemsRef.current = menuItems;
  }, [menuItems]);

  // Batch progress is throttled to one flush per animation frame (§10 keystroke budget):
  // events accumulate in a map, the newest tick per job wins, and one dispatch applies them.
  const pendingProgressRef = useRef(new Map<JobId, { done: number; total: number }>());
  const progressRafRef = useRef<number | null>(null);
  useEffect(() => {
    let unlistenProgress: UnlistenFn | null = null;
    let unlistenFinished: UnlistenFn | null = null;
    void subscribeOperationProgress((progress) => {
      pendingProgressRef.current.set(progress.jobId, {
        done: progress.done,
        total: progress.total,
      });
      if (progressRafRef.current === null) {
        progressRafRef.current = requestAnimationFrame(() => {
          progressRafRef.current = null;
          const updates = [...pendingProgressRef.current.entries()].map(
            ([jobId, value]) => ({ jobId, done: value.done, total: value.total }),
          );
          pendingProgressRef.current.clear();
          opsDispatch({ type: "jobProgress", updates });
        });
      }
    }).match((fn) => {
      unlistenProgress = fn;
    }, reportShellError);
    void subscribeOperationFinished((finished) => {
      opsDispatch({
        type: "jobFinished",
        jobId: finished.jobId,
        failures: finished.failures,
      });
      // Rows disappear only after success (§8): re-list the active Tab so trashed/deleted
      // rows leave and pasted/moved rows arrive, selection re-resolved by path.
      const active = tabById(stateRef.current, stateRef.current.activeId);
      if (active !== undefined) {
        if (active.browse.load.status === "ready") {
          revalidate(active.id, active.browse.location);
        } else {
          navigate(active.id, active.browse.location, "replace");
        }
      }
    }).match((fn) => {
      unlistenFinished = fn;
    }, reportShellError);
    return () => {
      if (unlistenProgress !== null) {
        unlistenProgress();
      }
      if (unlistenFinished !== null) {
        unlistenFinished();
      }
      if (progressRafRef.current !== null) {
        cancelAnimationFrame(progressRafRef.current);
        progressRafRef.current = null;
      }
    };
  }, [revalidate, navigate]);

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
      if (event.defaultPrevented) {
        return;
      }

      // The Action Menu is the topmost Escape layer (§5): while open it owns Up/Down,
      // Enter/Right (run), and Escape (close), ahead of Search Results, Selected Items,
      // and the window hide. stopPropagation keeps the Navigation Input's own Escape from
      // also firing.
      const currentOps = opsRef.current;
      if (currentOps.menu !== null) {
        const items = menuItemsRef.current;
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          closeActionMenu();
          return;
        }
        if (
          event.key === "ArrowDown" ||
          event.key === "ArrowUp" ||
          (event.ctrlKey && (event.key === "j" || event.key === "J")) ||
          (event.ctrlKey && (event.key === "k" || event.key === "K"))
        ) {
          event.preventDefault();
          event.stopPropagation();
          const delta =
            event.key === "ArrowDown" || event.key === "j" || event.key === "J"
              ? 1
              : -1;
          focusMenuItem(nextEnabledIndex(items, currentOps.menu.focusedIndex, delta));
          return;
        }
        if (event.key === "Enter" || event.key === "ArrowRight") {
          event.preventDefault();
          event.stopPropagation();
          const item = items[currentOps.menu.focusedIndex];
          if (item !== undefined) {
            runMenuItem(item);
          }
          return;
        }
        // Swallow the rest so the Browse list underneath never reacts while the menu owns
        // the keyboard.
        event.preventDefault();
        event.stopPropagation();
        return;
      }

      // The always-on Delete Permanently confirm (§8): Enter confirms, Escape cancels.
      if (currentOps.confirm !== null) {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          cancelDelete();
          return;
        }
        if (event.key === "Enter") {
          event.preventDefault();
          event.stopPropagation();
          confirmDelete();
          return;
        }
        return;
      }

      if (isEditableTarget(event.target)) {
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
          case "c":
          case "C":
            // Cmd+C copies Selected Items as file references (§8, point 1).
            if (!event.altKey) {
              event.preventDefault();
              copySelection();
            }
            return;
          case "v":
          case "V":
            // Cmd+V paste-copies; Cmd+Opt+V paste-moves and clears the clipboard (§8).
            event.preventDefault();
            if (event.altKey) {
              movePasteIntoLocation();
            } else {
              pasteIntoLocation();
            }
            return;
          case "k":
          case "K":
            // Cmd+K opens the one Action Menu (§5).
            event.preventDefault();
            openActionMenu("cmd_k");
            return;
          case "Backspace":
          case "Delete":
            // Cmd+Delete → Trash (no confirm); Opt+Cmd+Delete → confirm then delete (§8).
            event.preventDefault();
            if (event.altKey) {
              requestDeleteSelection();
            } else {
              trashSelection();
            }
            return;
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
    closeActionMenu,
    focusMenuItem,
    runMenuItem,
    cancelDelete,
    confirmDelete,
    copySelection,
    pasteIntoLocation,
    movePasteIntoLocation,
    trashSelection,
    requestDeleteSelection,
    openActionMenu,
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
    ops,
    menuItems,
    openRowMenu,
    focusMenuItem,
    runMenuItem,
    closeActionMenu,
    commitRename,
    cancelRename,
    confirmDelete,
    cancelDelete,
    dismissProblem,
  };
}
