import { useVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FileItem } from "../electron/types";
import { benchmarkItems, clampSelection, formatSize } from "./model";

type Mode = "recents" | "directory" | "benchmark";

interface TabState {
  id: number;
  title: string;
  mode: Mode;
  location: string;
  items: FileItem[];
  selection: number;
  history: string[];
  loading: boolean;
  error: string | null;
  warning: string | null;
}

const initialTab = (id = 1): TabState => ({
  id,
  title: "Recents",
  mode: "recents",
  location: "Recents",
  items: [],
  selection: -1,
  history: [],
  loading: true,
  error: null,
  warning: null,
});

function fileKind(item: FileItem): string {
  if (item.kind === "directory") return "Folder";
  const suffix = item.name.includes(".") ? item.name.split(".").pop() : "File";
  return suffix?.toUpperCase() || "File";
}

function dateLabel(value: number | null): string {
  return value === null ? "" : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(value);
}

export function App() {
  const [tabs, setTabs] = useState<TabState[]>([initialTab()]);
  const [activeId, setActiveId] = useState(1);
  const [pathValue, setPathValue] = useState("");
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; item: FileItem } | null>(null);
  const [quickLookPath, setQuickLookPath] = useState<string | null>(null);
  const nextTabId = useRef(2);
  const pathInput = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const active = tabs.find((tab) => tab.id === activeId) ?? tabs[0];

  const updateActive = useCallback((updater: (tab: TabState) => TabState) => {
    setTabs((current) => current.map((tab) => (tab.id === activeId ? updater(tab) : tab)));
  }, [activeId]);

  const refreshRecents = useCallback(async (tabId: number) => {
    setTabs((current) => current.map((tab) => tab.id === tabId ? { ...tab, loading: true, error: null } : tab));
    try {
      const result = await window.visualFiles.loadRecents();
      setTabs((current) => current.map((tab) => tab.id === tabId ? {
        ...tab,
        title: "Recents",
        mode: "recents",
        location: "Recents",
        items: result.items,
        selection: result.items.length ? 0 : -1,
        loading: false,
        warning: result.warning ?? null,
      } : tab));
    } catch (error) {
      setTabs((current) => current.map((tab) => tab.id === tabId ? {
        ...tab,
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      } : tab));
    }
  }, []);

  const loadPath = useCallback(async (input: string, recordHistory = true) => {
    const tabId = activeId;
    const previousLocation = active.mode === "directory" ? active.location : null;
    updateActive((tab) => ({ ...tab, loading: true, error: null }));
    try {
      const result = await window.visualFiles.loadPath(input);
      setTabs((current) => current.map((tab) => {
        if (tab.id !== tabId) return tab;
        const selection = result.selectPath
          ? result.items.findIndex((item) => item.path === result.selectPath)
          : result.items.length ? 0 : -1;
        return {
          ...tab,
          title: result.location.split("/").filter(Boolean).pop() || "/",
          mode: "directory",
          location: result.location,
          items: result.items,
          selection,
          history: recordHistory && previousLocation && previousLocation !== result.location
            ? [...tab.history, previousLocation]
            : tab.history,
          loading: false,
          warning: null,
        };
      }));
      setPathValue("");
    } catch (error) {
      updateActive((tab) => ({ ...tab, loading: false, error: error instanceof Error ? error.message : String(error) }));
    }
  }, [active, activeId, updateActive]);

  useEffect(() => {
    window.visualFiles.frontendReady();
    void refreshRecents(1);
    return window.visualFiles.onFocusPath(() => {
      requestAnimationFrame(() => requestAnimationFrame(() => {
        pathInput.current?.focus();
        pathInput.current?.select();
        window.visualFiles.visibleAndFocused(document.activeElement === pathInput.current);
      }));
    });
  }, [refreshRecents]);

  const virtualizer = useVirtualizer({
    count: active.items.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 38,
    overscan: 12,
  });

  useEffect(() => {
    if (active.mode !== "benchmark") return;
    requestAnimationFrame(() => requestAnimationFrame(() => window.visualFiles.benchmarkRendered(active.items.length)));
  }, [active.id, active.items.length, active.mode]);

  const moveSelection = useCallback((delta: number) => {
    updateActive((tab) => ({ ...tab, selection: clampSelection((tab.selection < 0 ? 0 : tab.selection) + delta, tab.items.length) }));
  }, [updateActive]);

  useEffect(() => {
    if (active.selection >= 0) virtualizer.scrollToIndex(active.selection, { align: "auto" });
  }, [active.selection, virtualizer]);

  const selected = active.items[active.selection] ?? null;

  const primaryAction = useCallback(async (item: FileItem | null) => {
    if (!item || item.synthetic || !item.path) return;
    if (item.kind === "directory") await loadPath(item.path);
    else await window.visualFiles.openPath(item.path);
  }, [loadPath]);

  const toggleQuickLook = useCallback(async (item: FileItem | null) => {
    if (quickLookPath) {
      await window.visualFiles.quickLook(null);
      setQuickLookPath(null);
      return;
    }
    if (!item?.path || item.kind !== "file") return;
    const result = await window.visualFiles.quickLook(item.path);
    if (result.open) setQuickLookPath(item.path);
  }, [quickLookPath]);

  const createTab = useCallback(() => {
    const id = nextTabId.current++;
    setTabs((current) => [...current, initialTab(id)]);
    setActiveId(id);
    setPathValue("");
    void refreshRecents(id);
    requestAnimationFrame(() => pathInput.current?.focus());
  }, [refreshRecents]);

  const closeTab = useCallback((id: number) => {
    if (tabs.length === 1) {
      const resetId = tabs[0].id;
      setTabs([initialTab(resetId)]);
      void refreshRecents(resetId);
      return;
    }
    setTabs((current) => {
      const index = current.findIndex((tab) => tab.id === id);
      const remaining = current.filter((tab) => tab.id !== id);
      if (id === activeId) setActiveId(remaining[Math.max(0, index - 1)].id);
      return remaining;
    });
  }, [activeId, refreshRecents, tabs]);

  const openBenchmark = useCallback(() => {
    const id = nextTabId.current++;
    const items = benchmarkItems();
    setTabs((current) => [...current, {
      id,
      title: "Benchmark 10k",
      mode: "benchmark",
      location: "Synthetic dataset",
      items,
      selection: 0,
      history: [],
      loading: false,
      error: null,
      warning: "Synthetic deterministic rows; no filesystem access.",
    }]);
    setActiveId(id);
  }, []);

  const goBack = useCallback(() => {
    const destination = active.history.at(-1);
    if (!destination) return;
    updateActive((tab) => ({ ...tab, history: tab.history.slice(0, -1) }));
    void loadPath(destination, false);
  }, [active.history, loadPath, updateActive]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const modifier = event.metaKey;
      if (modifier && event.key.toLowerCase() === "t") {
        event.preventDefault();
        createTab();
        return;
      }
      if (modifier && event.key.toLowerCase() === "w") {
        event.preventDefault();
        closeTab(activeId);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        if (contextMenu) setContextMenu(null);
        else if (quickLookPath) void toggleQuickLook(selected);
        else void window.visualFiles.hideWindow();
        return;
      }
      const inputHasText = document.activeElement === pathInput.current && pathValue.length > 0;
      if (event.key === "ArrowDown" || (event.ctrlKey && event.key.toLowerCase() === "j")) {
        if (inputHasText && event.key === "ArrowDown") return;
        event.preventDefault();
        listRef.current?.focus();
        moveSelection(1);
      } else if (event.key === "ArrowUp" || (event.ctrlKey && event.key.toLowerCase() === "k")) {
        if (inputHasText && event.key === "ArrowUp") return;
        event.preventDefault();
        listRef.current?.focus();
        moveSelection(-1);
      } else if (event.key === "ArrowRight") {
        if (inputHasText) return;
        event.preventDefault();
        void primaryAction(selected);
      } else if (event.key === "ArrowLeft") {
        if (inputHasText) return;
        event.preventDefault();
        goBack();
      } else if (event.key === "Enter") {
        event.preventDefault();
        if (pathValue.trim()) void loadPath(pathValue);
        else void primaryAction(selected);
      } else if (event.code === "Space" && document.activeElement !== pathInput.current) {
        event.preventDefault();
        void toggleQuickLook(selected);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [activeId, closeTab, contextMenu, createTab, goBack, loadPath, moveSelection, pathValue, primaryAction, quickLookPath, selected, toggleQuickLook]);

  const rows = virtualizer.getVirtualItems();
  const status = active.error
    ? active.error
    : active.loading
      ? "Loading..."
      : `${active.items.length.toLocaleString()} items`;

  return (
    <main className="window" onClick={() => contextMenu && setContextMenu(null)}>
      <header className="chrome">
        <div className="traffic-lights" aria-hidden="true"><i /><i /><i /></div>
        <div className="tabs" role="tablist">
          {tabs.map((tab) => (
            <button
              className={`tab ${tab.id === activeId ? "active" : ""}`}
              key={tab.id}
              role="tab"
              aria-selected={tab.id === activeId}
              onClick={(event) => { event.stopPropagation(); setActiveId(tab.id); }}
            >
              <span>{tab.title}</span>
              <span className="tab-close" role="button" aria-label={`Close ${tab.title}`} onClick={(event) => { event.stopPropagation(); closeTab(tab.id); }}>×</span>
            </button>
          ))}
          <button className="new-tab" aria-label="New tab" onClick={(event) => { event.stopPropagation(); createTab(); }}>+</button>
        </div>
      </header>

      <section className="toolbar">
        <button className="back" aria-label="Back" disabled={!active.history.length} onClick={goBack}>‹</button>
        <input
          ref={pathInput}
          value={pathValue}
          onChange={(event) => setPathValue(event.target.value)}
          placeholder="Paste an absolute path or ~/path"
          spellCheck={false}
          aria-label="Path Input"
        />
        <button className="benchmark" onClick={openBenchmark}>Benchmark 10k</button>
        {active.mode === "recents" && <button className="refresh" onClick={() => void refreshRecents(active.id)}>Refresh</button>}
      </section>

      <div className="column-header">
        <span>Name</span><span>Kind</span><span>Modified</span><span>Size</span>
      </div>
      <div className="list" ref={listRef} role="listbox" aria-label={active.location} tabIndex={0}>
        <div className="list-spacer" style={{ height: `${virtualizer.getTotalSize()}px` }}>
          {rows.map((virtualRow) => {
            const item = active.items[virtualRow.index];
            return (
              <div
                key={item.id}
                role="option"
                aria-selected={virtualRow.index === active.selection}
                className={`row ${virtualRow.index === active.selection ? "selected" : ""}`}
                style={{ transform: `translateY(${virtualRow.start}px)`, height: `${virtualRow.size}px` }}
                onClick={(event) => { event.stopPropagation(); updateActive((tab) => ({ ...tab, selection: virtualRow.index })); }}
                onDoubleClick={() => void primaryAction(item)}
                onContextMenu={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  updateActive((tab) => ({ ...tab, selection: virtualRow.index }));
                  setContextMenu({ x: event.clientX, y: event.clientY, item });
                }}
              >
                <span className="name"><span className="icon">{item.kind === "directory" ? "▸" : "·"}</span>{item.name}</span>
                <span>{fileKind(item)}</span>
                <span>{dateLabel(item.modifiedMs)}</span>
                <span>{formatSize(item.size)}</span>
              </div>
            );
          })}
        </div>
      </div>

      <footer className="status">
        <span title={active.location}>{active.location}</span>
        <span className={active.error ? "error" : ""}>{status}</span>
        {active.warning && <span className="warning" title={active.warning}>⚠</span>}
      </footer>

      {contextMenu && (
        <div className="context-menu" style={{ left: contextMenu.x, top: contextMenu.y }} onClick={(event) => event.stopPropagation()}>
          <button disabled={!contextMenu.item.path || contextMenu.item.synthetic} onClick={() => { void primaryAction(contextMenu.item); setContextMenu(null); }}>Open</button>
          <button disabled={!contextMenu.item.path} onClick={() => { if (contextMenu.item.path) void window.visualFiles.copyPath(contextMenu.item.path); setContextMenu(null); }}>Copy Path</button>
          <button disabled={!contextMenu.item.path || contextMenu.item.kind !== "file"} onClick={() => { void toggleQuickLook(contextMenu.item); setContextMenu(null); }}>Quick Look</button>
        </div>
      )}
    </main>
  );
}
