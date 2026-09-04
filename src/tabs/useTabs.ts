import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { type ResultAsync } from "neverthrow";

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
  TERMINAL_BUNDLE_IDS,
  type Slot,
} from "../operations/settings";
import type { SetSettingsOutcome } from "../settings/ipc";
import type { Settings } from "../settings/schema";
import {
  initialOpsState,
  opsReducer,
  type MenuItem,
  type MenuVia,
  type OpsState,
} from "../operations/state";
import {
  focusedItemOf,
  hasSelectedItems,
  initialBrowseState,
  itemAt,
  selectedPathsOf,
  type BrowseState,
  type LoadResult,
  type LoadedItems,
  type NavKind,
  type SelectMode,
} from "../browse/state";
import {
  listLocationFileNeighbor,
  listLocationInitial,
  listLocationSelectionPaths,
  listLocationWindow,
} from "../location/ipc";
import {
  directoryLocation,
  recentsLocation,
  type Location,
} from "../location/location";
import { getRecents } from "../location/recents";
import type {
  InitialListLocationResponse,
  Item,
  ListingSessionId,
  ListLocationWindowResponse,
} from "../location/schema";
import {
  rebindSearchMemory,
  recordSearchSignal,
  recordVisit,
  searchNameIndex,
  subscribeRankerReloaded,
  subscribeSearchQgramReady,
  type SearchHit,
  type SearchSignalKind,
} from "../search/ipc";
import {
  displayedHits,
  initialSearchState,
  type SearchState,
} from "../search/state";
import {
  quickLookHide,
  quickLookShow,
  quickLookUpdate,
  subscribeQuickLookClosed,
  subscribeQuickLookKey,
} from "../preview/quickLook";
import {
  copyToClipboard,
  readClipboardText,
  recordTelemetry,
  reportShellError,
  requestHide,
  requestQuit,
  subscribeRecentsUpdated,
  subscribeWindowShown,
  type ShellError,
} from "../shell";
import { entryPointLocation } from "./entryPoint";
import {
  BACKGROUND_RESET_MS,
  excursionsToReset,
  expiredTemporaryIds,
  lifetimeMs,
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
import { ROW_HEIGHT } from "../components/layout";

// Search tuning (SPEC §6, §10). The overlay shows the top ranked hits; a query is
// "slow" once it is still in flight after this many ms; telemetry is sampled to
// stay cheap (recorded when a query is slow-ish, else every Nth query).
const SEARCH_LIMIT = 50;
const SEARCH_SLOW_MS = 150;
const SEARCH_TELEMETRY_MS_THRESHOLD = 25;
const SEARCH_TELEMETRY_SAMPLE = 20;

// Recents loads and scrolls in pages of this size (SPEC §7): the first page paints from
// the cache, later pages arrive on scroll with no hard cap.
const RECENTS_BATCH = 100;

// WebContent holds only this many metadata-rich rows from a directory. Fetch the next
// window while the viewport is still this far from an edge, so wheel and key scrolling do
// not catch the asynchronous IPC boundary (issue #38).
const DIRECTORY_WINDOW_ITEMS = 2_048;
const DIRECTORY_WINDOW_PREFETCH = 512;

function directoryWindowOffset(total: number, preferredIndex: number): number {
  const count = Math.min(total, DIRECTORY_WINDOW_ITEMS);
  return Math.min(
    Math.max(0, preferredIndex - Math.floor(count / 2)),
    Math.max(0, total - count),
  );
}

function loadedInitial(response: InitialListLocationResponse): LoadedItems {
  return {
    items: response.items,
    offset: response.offset,
    total: response.total,
    sessionId: response.sessionId,
    focusIndex: response.focusIndex,
    selected: response.selected,
  };
}

function loadedWindow(
  response: ListLocationWindowResponse,
  initial: InitialListLocationResponse,
): LoadedItems {
  return {
    items: response.items,
    offset: response.offset,
    total: response.total,
    sessionId: response.sessionId,
    focusIndex: initial.focusIndex,
    selected: initial.selected,
  };
}

function loadedLocal(
  items: Item[],
  focusPath: string | null,
  selectedPaths: readonly string[],
): LoadedItems {
  const focusIndex =
    focusPath === null
      ? null
      : (() => {
          const index = items.findIndex((item) => item.path === focusPath);
          return index >= 0 ? index : null;
        })();
  const selected = selectedPaths.flatMap((path) => {
    const index = items.findIndex((item) => item.path === path);
    return index >= 0 ? [{ path, index }] : [];
  });
  return {
    items,
    offset: 0,
    total: items.length,
    sessionId: null,
    focusIndex,
    selected,
  };
}

// The kind of Tab a group boundary drop lands in — the signal that a drag
// crossed the Pinned/Temporary divide and must pin or unpin (§4, point 7).
export type TabGroup = "pinned" | "temporary";

// The live Quick Look session: the files-only list the panel was opened on, the current
// position within it, and the parallel Browse row index of each file so Up/Down can move the
// app's Focused Item in sync (SPEC §9). `null` whenever the panel is closed — the honest
// mirror the Escape order reads (kept truthful by the closed event, SPEC §5).
interface LocalQuickLookSession {
  kind: "local";
  paths: string[];
  rows: number[];
  index: number;
}

interface PagedQuickLookSession {
  kind: "paged";
  sessionId: ListingSessionId;
  row: number;
  request: number;
}

type QuickLookSession = LocalQuickLookSession | PagedQuickLookSession;

// The files-only rows of a Browse listing, in the given row order — the Quick Look list and
// its Focused-Item mapping (SPEC §9: "files only"). Directories are skipped.
function fileRowsOf(
  browse: BrowseState,
  rowOrder: Iterable<number>,
): { paths: string[]; rows: number[] } {
  if (browse.load.status !== "ready") {
    return { paths: [], rows: [] };
  }
  const paths: string[] = [];
  const rows: number[] = [];
  for (const index of rowOrder) {
    const item = itemAt(browse.load, index);
    if (item !== undefined && !item.isDirectory) {
      paths.push(item.path);
      rows.push(index);
    }
  }
  return { paths, rows };
}

export interface Tabs {
  state: TabsState;
  activeTab: Tab;
  activeBrowse: BrowseState;
  activeSearch: SearchState;
  // Browse interactions, always aimed at the active Tab.
  select: (index: number, mode: SelectMode) => void;
  activateItem: (item: Item) => void;
  onScrollTop: (top: number) => void;
  onVisibleRange: (first: number, last: number) => void;
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
  // The in-app Settings view (§12): whether it is open, and its open/close controls. Opened
  // from the Action Menu's Settings item and Cmd+, ; closed by Escape or its own control.
  settingsOpen: boolean;
  openSettings: () => void;
  closeSettings: () => void;
}

function isEditableTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.tagName === "INPUT" || target.isContentEditable)
  );
}

// The app-level Cmd shortcuts that stay live while an editable target (the search input)
// has focus: tab management and app navigation. Editing combos (Cmd+C/V/X/A/Z, Cmd+Delete)
// are deliberately absent so they keep reaching the input's own text handling.
function isEditablePassThroughShortcut(event: KeyboardEvent): boolean {
  if (!event.metaKey) {
    return false;
  }
  const digit = Number.parseInt(event.key, 10);
  if (!Number.isNaN(digit) && digit >= 1 && digit <= 9) {
    return true; // Cmd+1–8: activate tab N; Cmd+9: the last tab.
  }
  switch (event.key) {
    case "t":
    case "T": // Cmd+T: new Temporary Tab.
    case "w":
    case "W": // Cmd+W: close active tab.
    case "k":
    case "K": // Cmd+K: Action Menu.
    case "l":
    case "L": // Cmd+L: select the retained query (no editing meaning in an input).
    case "[": // Cmd+[: back.
    case "]": // Cmd+]: forward.
    case ",": // Cmd+,: Settings.
      return true;
    default:
      return false;
  }
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

export function useTabs(
  settings: Settings,
  saveSettings: (next: Settings) => ResultAsync<SetSettingsOutcome, ShellError>,
): Tabs {
  // The opening Tab is stamped at 0; the first show (or navigation) re-stamps it,
  // and it is the active Tab so lifetime expiry never touches it meanwhile.
  const [state, dispatch] = useReducer(tabsReducer, 0, createInitialTabsState);

  // The live settings (SPEC §12), kept in a ref so event handlers and callbacks read the
  // current values without re-subscribing when they change. The store re-pulls on the
  // `settings-changed` event, so this stays current across a Settings edit.
  const settingsRef = useRef(settings);
  useEffect(() => {
    settingsRef.current = settings;
  }, [settings]);

  // Whether the in-app Settings view is open (§12). A ref mirror lets the capture-phase
  // keyboard handler read it synchronously (it owns Escape while Settings is up).
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsOpenRef = useRef(settingsOpen);
  useEffect(() => {
    settingsOpenRef.current = settingsOpen;
  }, [settingsOpen]);
  const openSettings = useCallback((): void => {
    setSettingsOpen(true);
  }, []);
  const closeSettings = useCallback((): void => {
    setSettingsOpen(false);
  }, []);

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

  // The live Quick Look session (null when closed). A ref, not state: the Escape order and the
  // Space/arrow keys read it synchronously in the keydown handler, and its truth is owned by
  // the backend mirror + closed event, not by a render (SPEC §5, §9).
  const quickLookRef = useRef<QuickLookSession | null>(null);

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

  const windowRequestRef = useRef(
    new Map<TabId, { sessionId: ListingSessionId; offset: number }>(),
  );
  const retryListingRef = useRef<(tabId: TabId) => void>(() => undefined);

  const requestDirectoryWindow = useCallback(
    (
      tabId: TabId,
      sessionId: ListingSessionId,
      total: number,
      preferredIndex: number,
    ): void => {
      const offset = directoryWindowOffset(total, preferredIndex);
      const pending = windowRequestRef.current.get(tabId);
      if (pending?.sessionId === sessionId && pending.offset === offset) {
        return;
      }
      windowRequestRef.current.set(tabId, { sessionId, offset });
      void listLocationWindow(
        sessionId,
        offset,
        DIRECTORY_WINDOW_ITEMS,
      ).match(
        (response) => {
          const latest = windowRequestRef.current.get(tabId);
          if (latest?.sessionId !== sessionId || latest.offset !== offset) {
            return;
          }
          windowRequestRef.current.delete(tabId);
          dispatch({
            type: "browse",
            tabId,
            action: {
              type: "windowLoaded",
              sessionId: response.sessionId,
              items: response.items,
              offset: response.offset,
              total: response.total,
            },
          });
        },
        (error) => {
          const latest = windowRequestRef.current.get(tabId);
          if (latest?.sessionId === sessionId && latest.offset === offset) {
            windowRequestRef.current.delete(tabId);
          }
          if (error.code === "session-expired") {
            retryListingRef.current(tabId);
          }
        },
      );
    },
    [],
  );

  // Per-Tab search request id (cancel-on-newer, §10) and the pending slow-line
  // timer per Tab, plus a sampling counter for search telemetry.
  const searchSeqRef = useRef(new Map<TabId, number>());
  const slowTimerRef = useRef(new Map<TabId, ReturnType<typeof setTimeout>>());
  const searchCountRef = useRef(0);
  // Explicit New Tab actions are timed through the first committed entry-point frame.
  // Keeping this outside React state avoids adding a render to the measured path.
  const newTabTimingRef = useRef(new Map<TabId, number>());

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
      const tab = tabById(current, tabId);
      const restore =
        nav === "back"
          ? (tab?.browse.history[tab.browse.history.length - 1] ?? null)
          : nav === "forward"
            ? (tab?.browse.future[tab.browse.future.length - 1] ?? null)
            : null;
      const requestedFocusPath = focusPath ?? restore?.focusedPath ?? null;
      const requestedSelectedPaths =
        focusPath === undefined
          ? (restore?.selectedPaths ?? [])
          : [focusPath];
      const preferredIndex =
        restore === null ? null : Math.floor(restore.scrollTop / ROW_HEIGHT);
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
            focusScrollTop:
              focusPath !== undefined &&
              result.kind === "items" &&
              result.load.focusIndex !== null
                ? result.load.focusIndex * ROW_HEIGHT
                : null,
          },
        });
        const newTabStartedAt = newTabTimingRef.current.get(tabId);
        if (newTabStartedAt !== undefined) {
          newTabTimingRef.current.delete(tabId);
          // The first frame commits React state; the second observes the painted cached
          // collection and the focus request queued by `newTemporaryTab`.
          requestAnimationFrame(() => {
            requestAnimationFrame(() => {
              if (tabId === stateRef.current.activeId) {
                fireTelemetry("temporary_tab_first_frame", {
                  entry_point: location.kind,
                  focused: document.activeElement === inputRef.current,
                  duration_ms: Math.round(performance.now() - newTabStartedAt),
                });
              }
            });
          });
        }
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
                commit(recentsLocation, {
                  kind: "items",
                  load: loadedLocal(
                    response.items,
                    requestedFocusPath,
                    requestedSelectedPaths,
                  ),
                });
                break;
              case "empty":
                recentsMetaRef.current.set(tabId, { total: 0, loading: false });
                commit(recentsLocation, {
                  kind: "items",
                  load: loadedLocal([], null, []),
                });
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

      const listingStartedAt = performance.now();
      void listLocationInitial(
        target.path,
        tabId,
        seq,
        requestedFocusPath,
        requestedSelectedPaths,
        preferredIndex,
      ).match(
        (response) => {
          if (seq !== seqRef.current.get(tabId)) {
            return;
          }
          commit(directoryLocation(response.path), {
            kind: "items",
            load: loadedInitial(response),
          });
          // Two animation frames give React a paint opportunity after the listing state
          // commits. This is an upper-bound measurement from navigation through the first
          // rendered frame, including IPC, sorting, reconciliation, and virtualization.
          requestAnimationFrame(() => {
            requestAnimationFrame(() => {
              if (
                seq === seqRef.current.get(tabId) &&
                tabId === stateRef.current.activeId
              ) {
                fireTelemetry("location_first_frame", {
                  path: response.path,
                  count: response.total,
                  duration_ms: Math.round(performance.now() - listingStartedAt),
                });
              }
            });
          });
          if (!response.complete) {
            requestDirectoryWindow(
              tabId,
              response.sessionId,
              response.total,
              preferredIndex ?? response.focusIndex ?? response.offset,
            );
          }
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
    [nextSeq, requestDirectoryWindow],
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
            const currentTab = tabById(stateRef.current, tabId);
            const currentFocus = currentTab?.browse.focusedPath ?? null;
            const currentSelection =
              currentTab === undefined ? [] : selectedPathsOf(currentTab.browse);
            recentsMetaRef.current.set(tabId, { total, loading: false });
            dispatch({
              type: "browse",
              tabId,
              action: {
                type: "revalidated",
                location,
                load: loadedLocal(items, currentFocus, currentSelection),
                focusPath: null,
              },
            });
          }
        }, reportShellError);
        return;
      }
      const tab = tabById(stateRef.current, tabId);
      if (tab === undefined) {
        return;
      }
      const focusPath = tab.browse.focusedPath;
      const selectedPaths = selectedPathsOf(tab.browse);
      const preferredIndex = Math.floor(tab.browse.scrollTop / ROW_HEIGHT);
      void listLocationInitial(
        location.path,
        tabId,
        seq,
        focusPath,
        selectedPaths,
        preferredIndex,
      ).match((initial) => {
        if (seq !== seqRef.current.get(tabId)) {
          return;
        }
        const commit = (load: LoadedItems): void => {
          if (seq !== seqRef.current.get(tabId)) {
            return;
          }
          dispatch({
            type: "browse",
            tabId,
            action: {
              type: "revalidated",
              location: directoryLocation(initial.path),
              load,
              focusPath: null,
            },
          });
        };
        if (initial.complete) {
          commit(loadedInitial(initial));
          return;
        }
        const offset = directoryWindowOffset(initial.total, preferredIndex);
        void listLocationWindow(
          initial.sessionId,
          offset,
          DIRECTORY_WINDOW_ITEMS,
        ).match(
          (window) => {
            commit(loadedWindow(window, initial));
          },
          () => undefined,
        );
      }, () => undefined);
    },
    [nextSeq],
  );

  useEffect(() => {
    retryListingRef.current = (tabId): void => {
      const tab = tabById(stateRef.current, tabId);
      if (tab !== undefined) {
        revalidate(tabId, tab.browse.location);
      }
    };
  }, [revalidate]);

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

  // The single After Action policy (§12): consult the live table from Settings and hide the
  // window when the action calls for it.
  const afterAction = useCallback((action: AfterActionId): void => {
    const effect = afterEffectFor(settingsRef.current.afterAction, action);
    fireTelemetry("after_action", { action, effect });
    if (effect === "hide") {
      void requestHide("after-action").match(() => undefined, reportShellError);
    }
  }, []);

  // Search Memory learns only explicit eligible interactions. Most Browse actions have no
  // active query and therefore update query-independent usage alone; Search Result Reveal
  // passes its originating query explicitly. This keeps retained inactive input from
  // accidentally training unrelated later browsing.
  const recordLearnedSignal = useCallback(
    (path: string, kind: SearchSignalKind, query: string | null = null): void => {
      const normalizedQuery = query?.trim() === "" ? null : query;
      void recordSearchSignal(path, normalizedQuery, kind).match(
        () => undefined,
        reportShellError,
      );
    },
    [],
  );

  const openFile = useCallback(
    (path: string): void => {
      void openPath(path).match(() => {
        fireTelemetry("file_opened", { path });
        recordLearnedSignal(path, "completed_action");
        void recordVisit(path, "opened_file").match(
          () => undefined,
          reportShellError,
        );
        // Opening a file hides the window (§12 After Action default).
        afterAction("open_file");
      }, reportShellError);
    },
    [afterAction, recordLearnedSignal],
  );

  // Open the Action Menu on a specific Item (the "Show Action Menu" primary action, §5):
  // select the Item first when it is not already selected, then open the menu below the
  // Navigation Input (keyboard-style position, no anchor point).
  const openItemMenu = useCallback((item: Item): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined || active.browse.load.status !== "ready") {
      return;
    }
    const localIndex = active.browse.load.items.findIndex(
      (candidate) => candidate.path === item.path,
    );
    if (localIndex === -1) {
      return;
    }
    const index = active.browse.load.offset + localIndex;
    if (!active.browse.selected.has(index)) {
      dispatch({
        type: "browse",
        tabId: active.id,
        action: { type: "select", index, mode: "plain" },
      });
    }
    opsDispatch({ type: "openMenu", via: "row_button", x: null, y: null, focusedIndex: 0 });
    fireTelemetry("action_menu_opened", { via: "row_button" });
    recordLearnedSignal(item.path, "action_menu");
  }, [recordLearnedSignal]);

  const activateItem = useCallback(
    (item: Item): void => {
      const action = primaryActionFor(item, settingsRef.current);
      switch (action.kind) {
        case "enter":
          recordLearnedSignal(action.path, "completed_action");
          navigate(
            stateRef.current.activeId,
            directoryLocation(action.path),
            "enter",
          );
          // The user's primary Enter Location action on a directory (double-click, Enter/Right,
          // or the Action Menu's Open) — the one call site for user directory entry (§12
          // Enter Directory). Background hydration, Tab creation, lifecycle reset, and Open in
          // New Tab drive `navigate` directly, so they never reach this After Action.
          afterAction("enter_directory");
          break;
        case "open":
          openFile(action.path);
          break;
        case "menu":
          openItemMenu(item);
          break;
      }
    },
    [navigate, openFile, openItemMenu, afterAction, recordLearnedSignal],
  );

  const select = useCallback((index: number, mode: SelectMode): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    dispatch({
      type: "browse",
      tabId: stateRef.current.activeId,
      action: { type: "select", index, mode },
    });
    if (
      mode !== "range" ||
      active === undefined ||
      active.browse.load.status !== "ready" ||
      active.browse.load.sessionId === null
    ) {
      return;
    }
    const end = Math.min(Math.max(index, 0), active.browse.load.total - 1);
    const first = Math.min(active.browse.anchorIndex, end);
    const last = Math.max(active.browse.anchorIndex, end);
    const indices = Array.from({ length: last - first + 1 }, (_, offset) =>
      first + offset,
    );
    const sessionId = active.browse.load.sessionId;
    void listLocationSelectionPaths(sessionId, indices).match(
      (selected) => {
        dispatch({
          type: "browse",
          tabId: active.id,
          action: { type: "selectionPathsResolved", sessionId, selected },
        });
      },
      () => undefined,
    );
  }, []);

  const onScrollTop = useCallback((top: number): void => {
    scrollTopRef.current = top;
  }, []);

  const onVisibleRange = useCallback(
    (first: number, last: number): void => {
      const current = stateRef.current;
      const active = tabById(current, current.activeId);
      if (active === undefined || active.browse.load.status !== "ready") {
        return;
      }
      const load = active.browse.load;
      if (load.sessionId === null) {
        return;
      }
      const loadedEnd = load.offset + load.items.length;
      const safeFirst =
        load.offset === 0 ? 0 : load.offset + DIRECTORY_WINDOW_PREFETCH;
      const safeLast =
        loadedEnd >= load.total
          ? load.total
          : loadedEnd - DIRECTORY_WINDOW_PREFETCH;
      if (first >= safeFirst && last <= safeLast) {
        return;
      }
      const preferred = Math.floor((first + Math.max(first, last - 1)) / 2);
      const desiredOffset = directoryWindowOffset(load.total, preferred);
      if (desiredOffset === load.offset) {
        return;
      }
      requestDirectoryWindow(
        active.id,
        load.sessionId,
        load.total,
        preferred,
      );
    },
    [requestDirectoryWindow],
  );

  const focusRequestRef = useRef(new Map<TabId, number>());
  const focusDelta = useCallback((delta: number, extend: boolean): void => {
    const current = stateRef.current;
    const active = tabById(current, current.activeId);
    if (active === undefined || active.browse.load.status !== "ready") {
      return;
    }
    const load = active.browse.load;
    if (load.total === 0) {
      return;
    }
    const target = Math.min(
      Math.max(active.browse.focusedIndex + delta, 0),
      Math.max(0, load.total - 1),
    );
    if (itemAt(load, target) !== undefined || load.sessionId === null) {
      dispatch({
        type: "browse",
        tabId: active.id,
        action: { type: "focusDelta", delta, extend },
      });
      return;
    }
    const request = (focusRequestRef.current.get(active.id) ?? 0) + 1;
    focusRequestRef.current.set(active.id, request);
    const offset = directoryWindowOffset(load.total, target);
    void listLocationWindow(
      load.sessionId,
      offset,
      DIRECTORY_WINDOW_ITEMS,
    ).match(
      (response) => {
        if (focusRequestRef.current.get(active.id) !== request) {
          return;
        }
        dispatch({
          type: "browse",
          tabId: active.id,
          action: {
            type: "windowLoaded",
            sessionId: response.sessionId,
            items: response.items,
            offset: response.offset,
            total: response.total,
          },
        });
        dispatch({
          type: "browse",
          tabId: active.id,
          action: { type: "focusDelta", delta, extend },
        });
      },
      (error) => {
        if (error.code === "session-expired") {
          retryListingRef.current(active.id);
        }
      },
    );
  }, []);

  const withSelectedPaths = useCallback(
    (browse: BrowseState, run: (paths: string[]) => void): void => {
      const cached = selectedPathsOf(browse);
      if (
        cached.length === browse.selected.size ||
        browse.load.status !== "ready" ||
        browse.load.sessionId === null
      ) {
        run(cached);
        return;
      }
      const indices = [...browse.selected].sort((a, b) => a - b);
      void listLocationSelectionPaths(browse.load.sessionId, indices).match(
        (selected) => {
          run(selected.map((resolved) => resolved.path));
        },
        () => undefined,
      );
    },
    [],
  );

  // ---- Quick Look (§5, §9) ----------------------------------------------------------
  // Space toggles the native Quick Look panel over the current file list, focused at the
  // Focused Item; while open the frontend owns navigation and the panel stays front without
  // taking key focus (chunk A), so Up/Down move the app's Focused Item AND re-point the panel.

  // Open Quick Look on the active Browse Tab's file list (SPEC §9: files only). The list is
  // the Selected Items when a multi-selection contains files, otherwise every file of the
  // listing; it is anchored at the Focused Item. A no-op on a directory, an empty listing, or
  // in Search Mode (the Navigation Input owns Space there). Fire-and-forget — never awaited on
  // the Space path, so the dispatch stays within the ≤50 ms budget (SPEC §10).
  const openQuickLook = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (
      active === undefined ||
      active.search.mode === "search" ||
      active.browse.load.status !== "ready"
    ) {
      return;
    }
    const load = active.browse.load;
    const focusIndex = active.browse.focusedIndex;
    const focused = itemAt(load, focusIndex);
    if (focused === undefined || focused.isDirectory) {
      return;
    }
    const dispatchStartedAt = performance.now();
    const multiSelect = active.browse.selected.size > 1;
    let count: number;
    if (!multiSelect && load.sessionId !== null && load.total > load.items.length) {
      // A large Location keeps its order in Rust. WebContent retains the session id, current
      // row, and one metadata window; crossing that window asks Rust for the next file.
      quickLookRef.current = {
        kind: "paged",
        sessionId: load.sessionId,
        row: focusIndex,
        request: 0,
      };
      count = load.total;
    } else {
      const order: Iterable<number> = multiSelect
        ? [...active.browse.selected].sort((a, b) => a - b)
        : Array.from({ length: load.total }, (_, index) => index);
      let list = fileRowsOf(active.browse, order);
      if (!list.rows.includes(focusIndex)) {
        list = fileRowsOf(
          active.browse,
          Array.from({ length: load.total }, (_, index) => index),
        );
      }
      if (list.paths.length === 0) {
        return;
      }
      const position = Math.max(0, list.rows.indexOf(focusIndex));
      quickLookRef.current = {
        kind: "local",
        paths: list.paths,
        rows: list.rows,
        index: position,
      };
      count = list.paths.length;
    }
    void quickLookShow([focused.path], 0).match(
      () => {
        recordLearnedSignal(focused.path, "quick_look");
        fireTelemetry("quick_look_dispatch_completed", {
          count,
          duration_ms: Math.round(performance.now() - dispatchStartedAt),
        });
      },
      (error) => {
        // The panel never opened, so the mirror must not claim it did.
        quickLookRef.current = null;
        reportShellError(error);
      },
    );
  }, [recordLearnedSignal]);

  // Move through the open Quick Look list (Up/Down, Ctrl+J/K): re-point the panel and move the
  // app's Focused Item to the same file, leaving the Selected Items untouched so the original
  // selection is restored on close (SPEC §9). Navigation reuses chunk A's events — no new
  // telemetry here.
  const moveQuickLook = useCallback((delta: number): void => {
    const session = quickLookRef.current;
    if (session === null) {
      return;
    }
    if (session.kind === "paged") {
      const direction: -1 | 1 = delta < 0 ? -1 : 1;
      const active = tabById(stateRef.current, stateRef.current.activeId);
      if (
        active !== undefined &&
        active.browse.load.status === "ready" &&
        active.browse.load.sessionId === session.sessionId
      ) {
        const load = active.browse.load;
        const loadedFirst = load.offset;
        const loadedLast = load.offset + load.items.length - 1;
        let candidate = session.row + direction;
        while (candidate >= loadedFirst && candidate <= loadedLast) {
          const item = itemAt(load, candidate);
          if (item !== undefined && !item.isDirectory) {
            session.row = candidate;
            void quickLookUpdate([item.path], 0).match(
              () => undefined,
              reportShellError,
            );
            dispatch({
              type: "browse",
              tabId: active.id,
              action: { type: "refocus", index: candidate, path: item.path },
            });
            return;
          }
          candidate += direction;
        }
      }
      session.request += 1;
      const request = session.request;
      void listLocationFileNeighbor(
        session.sessionId,
        session.row,
        direction,
        DIRECTORY_WINDOW_ITEMS,
      ).match(
        (response) => {
          const current = quickLookRef.current;
          if (
            response === null ||
            current?.kind !== "paged" ||
            current.sessionId !== session.sessionId ||
            current.request !== request
          ) {
            return;
          }
          const item = response.items[response.focusIndex - response.offset];
          if (item === undefined) {
            return;
          }
          current.row = response.focusIndex;
          void quickLookUpdate([item.path], 0).match(
            () => undefined,
            reportShellError,
          );
          const tabId = stateRef.current.activeId;
          dispatch({
            type: "browse",
            tabId,
            action: {
              type: "windowLoaded",
              sessionId: response.sessionId,
              items: response.items,
              offset: response.offset,
              total: response.total,
            },
          });
          dispatch({
            type: "browse",
            tabId,
            action: {
              type: "refocus",
              index: response.focusIndex,
              path: item.path,
            },
          });
        },
        (error) => {
          if (error.code === "session-expired") {
            quickLookRef.current = null;
            void quickLookHide().match(() => undefined, reportShellError);
            retryListingRef.current(stateRef.current.activeId);
          }
        },
      );
      return;
    }
    const next = Math.min(
      Math.max(session.index + delta, 0),
      session.paths.length - 1,
    );
    if (next === session.index) {
      return;
    }
    session.index = next;
    const path = session.paths[next];
    if (path === undefined) {
      return;
    }
    void quickLookUpdate([path], 0).match(
      () => undefined,
      reportShellError,
    );
    const row = session.rows[next];
    if (row !== undefined) {
      dispatch({
        type: "browse",
        tabId: stateRef.current.activeId,
        action: { type: "refocus", index: row, path },
      });
    }
  }, []);

  // Close Quick Look programmatically (Space again, or the Escape order). The Focused Item is
  // already on the last file and the selection preserved, so nothing is restored here.
  const closeQuickLook = useCallback((): void => {
    if (quickLookRef.current === null) {
      return;
    }
    quickLookRef.current = null;
    void quickLookHide().match(() => undefined, reportShellError);
  }, []);

  // Sampled search telemetry: record only when a query was slow-ish or on every
  // Nth query, so the hot typing path stays cheap (§10, telemetry point 10).
  const fireSearchTelemetry = useCallback(
    (
      queryLen: number,
      results: number,
      durationMs: number,
      backendDurationMs: number,
      stage: string,
      complete: boolean,
      qgramReady: boolean,
      scanned: number,
    ): void => {
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
          backend_duration_ms: backendDurationMs,
          stage,
          complete,
          qgram_ready: qgramReady,
          scanned,
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
      const current = stateRef.current;
      const active = tabById(current, tabId);
      const currentLocation =
        active?.browse.location.kind === "directory"
          ? active.browse.location.path
          : null;
      const pinnedPaths = current.tabs.filter(isPinned).map((tab) => tab.anchorPath);
      void searchNameIndex(
        query,
        SEARCH_LIMIT,
        currentLocation,
        pinnedPaths,
        (wave) => {
          if (searchSeqRef.current.get(tabId) !== seq) {
            return;
          }
          fireSearchTelemetry(
            query.length,
            wave.hits.length,
            performance.now() - started,
            wave.backendDurationMs,
            wave.stage,
            wave.complete,
            wave.qgramReady,
            wave.scanned,
          );
          dispatch({
            type: "search",
            tabId,
            action: { type: "resultsWave", query, wave },
          });
          requestAnimationFrame(() => {
            if (searchSeqRef.current.get(tabId) !== seq) {
              return;
            }
            fireTelemetry("search_wave_painted", {
              query_len: query.length,
              stage: wave.stage,
              complete: wave.complete,
              qgram_ready: wave.qgramReady,
              results: wave.hits.length,
              backend_duration_ms: wave.backendDurationMs,
              duration_ms: Math.round(performance.now() - started),
            });
          });
          if (wave.complete) {
            clearSlowTimer(tabId);
          }
        },
      ).match(
        () => {
          if (searchSeqRef.current.get(tabId) === seq) {
            clearSlowTimer(tabId);
          }
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

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void subscribeSearchQgramReady(() => {
      const current = stateRef.current;
      const active = tabById(current, current.activeId);
      if (
        active !== undefined &&
        active.search.mode === "search" &&
        active.search.query.trim() !== ""
      ) {
        runSearch(active.id, active.search.query);
      }
    }).match((fn) => {
      unlisten = fn;
    }, reportShellError);
    return () => {
      if (unlisten !== null) {
        unlisten();
      }
    };
  }, [runSearch]);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void subscribeRankerReloaded(() => {
      const current = stateRef.current;
      const active = tabById(current, current.activeId);
      if (active?.search.mode === "search" && active.search.query.trim() !== "") {
        runSearch(active.id, active.search.query);
      }
    }).match(
      (stop) => {
        unlisten = stop;
      },
      reportShellError,
    );
    return () => {
      unlisten?.();
    };
  }, [runSearch]);

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
    const focused = itemAt(active.browse.load, active.browse.focusedIndex);
    if (focused !== undefined) {
      activateItem(focused);
    }
  }, [activateItem]);

  const goBack = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    const entry = active?.browse.history[active.browse.history.length - 1];
    if (active !== undefined && entry !== undefined) {
      navigate(active.id, entry.location, "back");
      // User-initiated Back is Navigation (§12).
      afterAction("navigation");
    }
  }, [navigate, afterAction]);

  const goForward = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    const entry = active?.browse.future[active.browse.future.length - 1];
    if (active !== undefined && entry !== undefined) {
      navigate(active.id, entry.location, "forward");
      // User-initiated Forward is Navigation (§12).
      afterAction("navigation");
    }
  }, [navigate, afterAction]);

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
      recordLearnedSignal(hit.path, "completed_action", origin.search.query);

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
      // A Search Result Reveal is Navigation (§12). It fires once here regardless of which
      // routing branch above ran (activate a Pinned Tab, reuse the Temporary origin, or open a
      // Temporary Tab) — none of those helpers apply an After Action themselves.
      afterAction("navigation");
    },
    [
      activateTab,
      navigate,
      openOrReuseTemporary,
      clearSlowTimer,
      afterAction,
      recordLearnedSignal,
    ],
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
          // The user clicking an active Pinned Tab to return it from its Excursion to its
          // Anchor is Navigation (§12). The automatic background reset shares the "reset"
          // NavKind but drives `navigate` directly, so it never fires this After Action.
          afterAction("navigation");
        }
        return;
      }
      activateTab(id);
    },
    [activateTab, navigate, afterAction],
  );

  const newTemporaryTab = useCallback((): void => {
    const current = stateRef.current;
    const nowMs = Date.now();
    const id = freshTabId();
    newTabTimingRef.current.set(id, performance.now());
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
    navigate(id, entryPointLocation(settingsRef.current.defaultEntryPoint), "replace");
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
      navigate(
        replacement.id,
        entryPointLocation(settingsRef.current.defaultEntryPoint),
        "replace",
      );
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
    withSelectedPaths(active.browse, (paths) => {
      if (paths.length === 0) {
        return;
      }
      opsDispatch({ type: "setClipboard", paths });
      void copyToClipboard(paths.join("\n")).match(() => {
        for (const path of paths) {
          recordLearnedSignal(path, "completed_action");
        }
        fireTelemetry("clipboard_copied", { count: paths.length });
        afterAction("copy_file");
      }, reportShellError);
    });
  }, [afterAction, withSelectedPaths, recordLearnedSignal]);

  // Copy Path: textual only (§8) — the path(s) to the system clipboard, newline-separated.
  // Does not touch the in-app file-reference clipboard. Copy Path hides the window (§12).
  const copyPath = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    withSelectedPaths(active.browse, (paths) => {
      if (paths.length === 0) {
        return;
      }
      void copyToClipboard(paths.join("\n")).match(() => {
        for (const path of paths) {
          recordLearnedSignal(path, "completed_action");
        }
        afterAction("copy_path");
      }, reportShellError);
    });
  }, [afterAction, withSelectedPaths, recordLearnedSignal]);

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
    withSelectedPaths(active.browse, (paths) => {
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
    });
  }, [afterAction, withSelectedPaths]);

  // Opt+Cmd+Delete / Delete Permanently: always opens the confirm (§8, no "don't ask
  // again"); the delete itself runs only on confirm.
  const requestDeleteSelection = useCallback((): void => {
    const active = tabById(stateRef.current, stateRef.current.activeId);
    if (active === undefined) {
      return;
    }
    withSelectedPaths(active.browse, (paths) => {
      if (paths.length > 0) {
        opsDispatch({ type: "openConfirm", paths });
      }
    });
  }, [withSelectedPaths]);

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
          void rebindSearchMemory(rename.path, newPath).match(
            () => undefined,
            reportShellError,
          );
          recordLearnedSignal(newPath, "completed_action");
          opsDispatch({ type: "cancelRename" });
          if (location !== undefined) {
            navigate(activeId, location, "replace", newPath);
          }
          // Rename fires its After Action only on a successful renameItem (§12): the collision
          // and other-failure branches below deliberately never reach it.
          afterAction("rename");
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
    [navigate, afterAction, recordLearnedSignal],
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

  // Resolve a Terminal/Editor slot (§8): the configured bundle id when its app is still
  // installed, otherwise the first installed app in the priority list — persisted on first
  // resolve. `null` means none is installed, so the caller keeps the action visible and routes
  // to Settings (§8: a configured-but-missing app never silently swaps to another).
  const resolveSlot = useCallback(
    async (slot: Slot): Promise<string | null> => {
      const current = settingsRef.current;
      const existing =
        slot === "terminal" ? current.terminalBundleId : current.editorBundleId;
      if (existing !== null) {
        // Verify the recorded app is still installed (SPEC §8). A single-id probe returns the
        // id when installed, else `null`; a probe failure is treated as not installed so the
        // action routes to Settings rather than dispatching to a missing app. The configured id
        // is left untouched so Settings can still show it as "not installed".
        return resolveInstalledBundle([existing]).match(
          (value) => (value === existing ? existing : null),
          (error) => {
            reportShellError(error);
            return null;
          },
        );
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
        // Persist the auto-seeded slot through the settings store so a later Open in
        // Terminal/Editor never re-probes (SPEC §8). The changed event re-pulls settingsRef.
        const next: Settings =
          slot === "terminal"
            ? { ...current, terminalBundleId: resolved }
            : { ...current, editorBundleId: resolved };
        settingsRef.current = next;
        void saveSettings(next).match(() => undefined, reportShellError);
      }
      return resolved;
    },
    [saveSettings],
  );

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
        // A configured-but-missing (or unset) app routes to Settings on invocation (§8).
        openSettings();
        return;
      }
      void openInApp(target, bundle).match(
        () => {
          recordLearnedSignal(item.path, "completed_action");
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
  }, [afterAction, resolveSlot, openSettings, recordLearnedSignal]);

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
        // A configured-but-missing (or unset) app routes to Settings on invocation (§8).
        openSettings();
        return;
      }
      void openInApp(target, bundle).match(
        () => {
          recordLearnedSignal(item.path, "completed_action");
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
  }, [afterAction, resolveSlot, openSettings, recordLearnedSignal]);

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
        recordLearnedSignal(item.path, "completed_action");
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
  }, [afterAction, recordLearnedSignal]);

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
    recordLearnedSignal(item.path, "completed_action");
    fireTelemetry("tab_created", { kind: "temporary" });
    afterAction("open_in_new_tab");
  }, [navigate, afterAction, recordLearnedSignal]);

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
    const active = tabById(stateRef.current, stateRef.current.activeId);
    const item = active === undefined ? undefined : focusedItemOf(active.browse);
    if (active !== undefined && item !== undefined && active.browse.selected.size > 0) {
      recordLearnedSignal(item.path, "action_menu");
    }
    opsDispatch({ type: "openMenu", via, x: null, y: null, focusedIndex: 0 });
    fireTelemetry("action_menu_opened", { via });
  }, [recordLearnedSignal]);

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
      const item = itemAt(active.browse.load, index);
      if (item !== undefined) {
        recordLearnedSignal(item.path, "action_menu");
      }
      opsDispatch({ type: "openMenu", via, x, y, focusedIndex: 0 });
      fireTelemetry("action_menu_opened", { via });
    },
    [recordLearnedSignal],
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
    const selCount = browse?.selected.size ?? 0;
    if (browse === undefined || selCount === 0) {
      // Application actions (§5): reachable via Cmd+K with an empty selection.
      const menu = strings.operations.menu;
      return [
        { id: "newTab", label: menu.newTab, disabled: false, run: newTemporaryTab },
        { id: "pastePath", label: menu.pastePath, disabled: false, run: pastePath },
        { id: "refresh", label: menu.refresh, disabled: false, run: refresh },
        { id: "settings", label: menu.settings, disabled: false, run: openSettings },
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
    // Quick Look acts on files only (§9): enabled when the Focused Item is a file.
    const focused = focusedItemOf(browse);
    const canQuickLook = focused !== undefined && !focused.isDirectory;
    return [
      { id: "open", label: menu.open, disabled: false, run: openSelected },
      {
        id: "quickLook",
        label: menu.quickLook,
        disabled: !canQuickLook,
        run: openQuickLook,
      },
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
    openSettings,
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
    openQuickLook,
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
        navigate(
          activeId,
          entryPointLocation(settingsRef.current.defaultEntryPoint),
          "replace",
        );
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
      const expired = expiredTemporaryIds(
        current.tabs,
        current.activeId,
        nowMs,
        lifetimeMs(settingsRef.current.temporaryTabLifetime),
      );
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

  // Keep the Quick Look mirror truthful (SPEC §5): when the panel is closed by its own close
  // control rather than by the app, chunk A emits the closed event; drop the session so the
  // next Escape falls through to the layer below instead of being swallowed by a stale flag.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void subscribeQuickLookClosed(() => {
      quickLookRef.current = null;
    }).match((fn) => {
      unlisten = fn;
    }, reportShellError);
    return () => {
      if (unlisten !== null) {
        unlisten();
      }
    };
  }, []);

  // QLPreviewPanel is the key AppKit window while visible, so its local event monitor forwards
  // the Quick Look-only keyboard contract here instead of relying on WebView key delivery.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    void subscribeQuickLookKey((key) => {
      if (quickLookRef.current === null) {
        return;
      }
      switch (key) {
        case "next":
          moveQuickLook(1);
          break;
        case "previous":
          moveQuickLook(-1);
          break;
        case "close":
          closeQuickLook();
          break;
      }
    }).match((fn) => {
      unlisten = fn;
    }, reportShellError);
    return () => {
      if (unlisten !== null) {
        unlisten();
      }
    };
  }, [closeQuickLook, moveQuickLook]);

  // Combined keyboard contract: Tab shortcuts plus the Browse-mode keys, in the
  // capture phase so Escape can preventDefault before shell.ts's bubble hide.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (event.defaultPrevented) {
        return;
      }

      // Quick Look is the topmost layer (§5 Escape order): while open it owns Up/Down and
      // Ctrl+J/K (move through the files), Space and Escape (close), and swallows the rest so
      // the Browse list underneath never reacts. Its truth is the `quickLookRef` mirror.
      if (quickLookRef.current !== null) {
        event.preventDefault();
        event.stopPropagation();
        if (event.key === "Escape" || event.key === " ") {
          closeQuickLook();
        } else if (
          event.key === "ArrowDown" ||
          (event.ctrlKey && (event.key === "j" || event.key === "J"))
        ) {
          moveQuickLook(1);
        } else if (
          event.key === "ArrowUp" ||
          (event.ctrlKey && (event.key === "k" || event.key === "K"))
        ) {
          moveQuickLook(-1);
        }
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

      // The Settings view (§12) is a modal overlay above the Browse surface. Its Escape is
      // handled here — before the editable-target early-return, so it closes even while a
      // Settings field has focus, and above Search Results / the window hide in the Escape
      // order (SPEC §5). It sits below Quick Look / Action Menu / confirm only in code; those
      // never coexist with Settings. Every other key falls through to the form untouched,
      // while app shortcuts are swallowed so Settings stays modal.
      if (settingsOpenRef.current) {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          closeSettings();
        }
        return;
      }

      // In an editable target (the search input) only the allowlisted app shortcuts are
      // intercepted; every other key — plain text, arrows, and editing combos like
      // Cmd+C/V/X/A/Z and Cmd+Delete — falls through to the input untouched.
      if (
        isEditableTarget(event.target) &&
        !isEditablePassThroughShortcut(event)
      ) {
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
          case ",":
            // Cmd+, opens the Settings view (§12), the macOS convention.
            event.preventDefault();
            openSettings();
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
        case " ":
          // Space toggles Quick Look on the Focused Item (§5, §9): a no-op on a directory.
          event.preventDefault();
          openQuickLook();
          break;
        case "ArrowLeft":
          event.preventDefault();
          goBack();
          break;
        case "Escape": {
          const active = tabById(stateRef.current, stateRef.current.activeId);
          if (active !== undefined && hasSelectedItems(active.browse)) {
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
    openQuickLook,
    moveQuickLook,
    closeQuickLook,
    openSettings,
    closeSettings,
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
    onVisibleRange,
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
    settingsOpen,
    openSettings,
    closeSettings,
  };
}
