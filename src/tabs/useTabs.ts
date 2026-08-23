import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { openPath, primaryActionFor } from "../browse/actions";
import {
  hasSelectionBeyondFocus,
  initialBrowseState,
  type BrowseState,
  type NavKind,
  type SelectMode,
} from "../browse/state";
import { homeDirectory, listLocation } from "../location/ipc";
import type { Item } from "../location/schema";
import {
  copyToClipboard,
  recordTelemetry,
  reportShellError,
  requestHide,
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

// The kind of Tab a group boundary drop lands in — the signal that a drag
// crossed the Pinned/Temporary divide and must pin or unpin (§4, point 7).
export type TabGroup = "pinned" | "temporary";

export interface Tabs {
  state: TabsState;
  activeTab: Tab;
  activeBrowse: BrowseState;
  // Browse interactions, always aimed at the active Tab.
  select: (index: number, mode: SelectMode) => void;
  activateItem: (item: Item) => void;
  onScrollTop: (top: number) => void;
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

  // Resolved home directory backing the Default Entry Point seam.
  const homeRef = useRef("");

  // Per-Tab monotonic request id: only the newest listing of a given Tab may
  // commit, so a background revalidation never clobbers a live navigation (§10).
  const seqRef = useRef(new Map<TabId, number>());
  const nextSeq = useCallback((id: TabId): number => {
    const seq = (seqRef.current.get(id) ?? 0) + 1;
    seqRef.current.set(id, seq);
    return seq;
  }, []);

  const navigate = useCallback(
    (tabId: TabId, target: string, nav: NavKind): void => {
      const seq = nextSeq(tabId);
      const current = stateRef.current;
      const originScrollTop =
        tabId === current.activeId
          ? scrollTopRef.current
          : (tabById(current, tabId)?.browse.scrollTop ?? 0);
      void listLocation(target).match(
        (response) => {
          if (seq !== seqRef.current.get(tabId)) {
            return;
          }
          dispatch({
            type: "browse",
            tabId,
            action: {
              type: "listed",
              location: response.path,
              items: response.items,
              nav,
              originScrollTop,
            },
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

  // Re-list the current Location in the background, keeping the Focused Item put.
  const revalidate = useCallback(
    (tabId: TabId, location: string): void => {
      const seq = nextSeq(tabId);
      void listLocation(location).match(
        (response) => {
          if (seq !== seqRef.current.get(tabId)) {
            return;
          }
          dispatch({
            type: "browse",
            tabId,
            action: {
              type: "revalidated",
              location: response.path,
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

  const openFile = useCallback((path: string): void => {
    void openPath(path).match(() => {
      fireTelemetry("file_opened", { path });
    }, reportShellError);
  }, []);

  const activateItem = useCallback(
    (item: Item): void => {
      const action = primaryActionFor(item);
      switch (action.kind) {
        case "enter":
          navigate(stateRef.current.activeId, action.path, "enter");
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
          navigate(id, tab.anchorPath, "reset");
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
    navigate(id, defaultEntryPoint(homeRef.current), "replace");
    fireTelemetry("tab_created", { kind: "temporary" });
  }, [navigate]);

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
      navigate(replacement.id, defaultEntryPoint(homeRef.current), "replace");
    }
    fireTelemetry("tab_closed", { kind: tab.kind });
    seqRef.current.delete(id);
  }, [navigate]);

  const pinTab = useCallback((id: TabId): void => {
    const current = stateRef.current;
    const tab = tabById(current, id);
    if (
      tab === undefined ||
      tab.kind !== "temporary" ||
      tab.browse.load.status !== "ready"
    ) {
      return;
    }
    // Pinning a Location an Anchor already holds is a silent no-op (§4).
    const anchorPath = tab.browse.location;
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
    if (tab === undefined || tab.browse.location === "") {
      return;
    }
    void copyToClipboard(tab.browse.location).match(
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
    } else if (load.status === "error") {
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
      const home = (await homeDirectory()).match(
        (value) => value,
        () => "",
      );
      homeRef.current = home;
      const persisted = (await loadPinnedTabs()).match(
        (value) => value,
        (error) => {
          reportShellError(error);
          return [];
        },
      );
      if (persisted.length === 0) {
        // Keep the initial Temporary Tab; send it to the Default Entry Point.
        navigate(stateRef.current.activeId, defaultEntryPoint(home), "replace");
        hydratedRef.current = true;
        return;
      }
      const pinnedTabs: PinnedTab[] = persisted.map((entry) => ({
        kind: "pinned",
        id: freshTabId(),
        anchorPath: entry.anchorPath,
        customName: entry.customName,
        browse: initialBrowseState,
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
        navigate(pinnedTab.id, pinnedTab.anchorPath, "replace");
      }
    })();
  }, [navigate]);

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
            navigate(pinnedTab.id, pinnedTab.anchorPath, "reset");
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
  }, [navigate]);

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
    select,
    activateItem,
    onScrollTop,
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
