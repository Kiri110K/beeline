import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  FormEvent,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

type Item = {
  path: string;
  name: string;
  kind: string;
  modified_ms: number | null;
  size: number | null;
  is_dir: boolean;
};

type LoadResult = {
  location: string;
  items: Item[];
  selected_path: string | null;
  duration_ms: number;
};

type TabMode = "recents" | "directory" | "benchmark";

type Tab = {
  id: number;
  title: string;
  mode: TabMode;
  location: string;
  pathDraft: string;
  items: Item[];
  selected: number;
  history: string[];
  loading: boolean;
  error: string | null;
};

type ContextMenu = { x: number; y: number; item: Item } | null;

let nextTabId = 2;
let frontendReadyLogged = false;
const loggedBenchmarkTabs = new Set<number>();

function emptyRecentsTab(id: number): Tab {
  return {
    id,
    title: "Recents",
    mode: "recents",
    location: "Recents",
    pathDraft: "",
    items: [],
    selected: 0,
    history: [],
    loading: true,
    error: null,
  };
}

function benchmarkItems(): Item[] {
  return Array.from({ length: 10_000 }, (_, index) => {
    const ordinal = String(index + 1).padStart(5, "0");
    const isDir = index % 11 === 0;
    return {
      path: `/benchmark/${isDir ? "folder" : "file"}-${ordinal}${isDir ? "" : ".txt"}`,
      name: `${isDir ? "folder" : "file"}-${ordinal}${isDir ? "" : ".txt"}`,
      kind: isDir ? "Folder" : "Text",
      modified_ms: Date.UTC(2026, 0, 1) - index * 60_000,
      size: isDir ? null : 512 + ((index * 7919) % 8_000_000),
      is_dir: isDir,
    };
  });
}

function formatSize(size: number | null) {
  if (size == null) return "--";
  if (size < 1_000) return `${size} B`;
  if (size < 1_000_000) return `${(size / 1_000).toFixed(1)} KB`;
  return `${(size / 1_000_000).toFixed(1)} MB`;
}

function formatDate(value: number | null) {
  if (value == null) return "--";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function App() {
  const [tabs, setTabs] = useState<Tab[]>([emptyRecentsTab(1)]);
  const [activeId, setActiveId] = useState(1);
  const [contextMenu, setContextMenu] = useState<ContextMenu>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const activeTab = tabs.find((tab) => tab.id === activeId) ?? tabs[0];

  const updateTab = useCallback((id: number, update: Partial<Tab> | ((tab: Tab) => Partial<Tab>)) => {
    setTabs((current) =>
      current.map((tab) => {
        if (tab.id !== id) return tab;
        return { ...tab, ...(typeof update === "function" ? update(tab) : update) };
      }),
    );
  }, []);

  const loadRecents = useCallback(
    async (id: number) => {
      updateTab(id, { loading: true, error: null, mode: "recents", location: "Recents" });
      try {
        const result = await invoke<LoadResult>("load_recents");
        updateTab(id, {
          title: "Recents",
          items: result.items,
          selected: 0,
          loading: false,
          error: null,
        });
      } catch (error) {
        updateTab(id, { loading: false, error: String(error), items: [] });
      }
    },
    [updateTab],
  );

  const loadPath = useCallback(
    async (id: number, path: string, addHistory: boolean) => {
      updateTab(id, { loading: true, error: null });
      try {
        const result = await invoke<LoadResult>("load_path", { input: path });
        updateTab(id, (tab) => ({
          title: result.location.split("/").filter(Boolean).at(-1) ?? result.location,
          mode: "directory",
          location: result.location,
          pathDraft: result.location,
          items: result.items,
          selected: Math.max(
            0,
            result.selected_path ? result.items.findIndex((item) => item.path === result.selected_path) : 0,
          ),
          history: addHistory && tab.mode === "directory" ? [...tab.history, tab.location] : tab.history,
          loading: false,
          error: null,
        }));
      } catch (error) {
        updateTab(id, { loading: false, error: String(error) });
      }
    },
    [updateTab],
  );

  useEffect(() => {
    if (!frontendReadyLogged) {
      frontendReadyLogged = true;
      // A hidden WKWebView throttles requestAnimationFrame. Log from the committed effect so
      // hidden-ready time remains observable before the first shortcut opens the window.
      void invoke("frontend_ready");
      void loadRecents(1);
    }
    const unlisten = listen("focus-path-input", () => {
      requestAnimationFrame(() => {
        inputRef.current?.focus();
        inputRef.current?.select();
      });
    });
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, [loadRecents]);

  const createTab = useCallback(() => {
    const id = nextTabId++;
    setTabs((current) => [...current, emptyRecentsTab(id)]);
    setActiveId(id);
    setContextMenu(null);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
    void loadRecents(id);
  }, [loadRecents]);

  const createBenchmarkTab = useCallback(() => {
    const id = nextTabId++;
    const items = benchmarkItems();
    setTabs((current) => [
      ...current,
      {
        id,
        title: "Benchmark 10k",
        mode: "benchmark",
        location: "Benchmark 10k",
        pathDraft: "",
        items,
        selected: 0,
        history: [],
        loading: false,
        error: null,
      },
    ]);
    setActiveId(id);
  }, []);

  const closeTab = useCallback((id: number) => {
    if (tabs.length === 1) {
      const reset = emptyRecentsTab(tabs[0].id);
      setTabs([reset]);
      void loadRecents(reset.id);
      requestAnimationFrame(() => inputRef.current?.focus());
      return;
    }
    setTabs((current) => {
      const index = current.findIndex((tab) => tab.id === id);
      const next = current.filter((tab) => tab.id !== id);
      if (id === activeId) setActiveId(next[Math.max(0, index - 1)].id);
      return next;
    });
  }, [activeId, loadRecents, tabs]);

  const selectOffset = useCallback(
    (offset: number) => {
      updateTab(activeId, (tab) => ({
        selected: Math.max(0, Math.min(tab.items.length - 1, tab.selected + offset)),
      }));
    },
    [activeId, updateTab],
  );

  const selectedItem = activeTab.items[activeTab.selected] ?? null;

  const primaryAction = useCallback(
    async (item: Item | null) => {
      if (!item || activeTab.mode === "benchmark") return;
      if (item.is_dir) await loadPath(activeId, item.path, true);
      else await invoke("open_path", { path: item.path });
    },
    [activeId, activeTab.mode, loadPath],
  );

  const goBack = useCallback(() => {
    if (activeTab.mode !== "directory" || activeTab.history.length === 0) return;
    const previous = activeTab.history.at(-1)!;
    updateTab(activeId, (tab) => ({ history: tab.history.slice(0, -1) }));
    void loadPath(activeId, previous, false);
  }, [activeId, activeTab, loadPath, updateTab]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const editing = target?.tagName === "INPUT" || target?.tagName === "TEXTAREA";
      if (event.metaKey && event.key.toLowerCase() === "t") {
        event.preventDefault();
        createTab();
        return;
      }
      if (event.metaKey && event.key.toLowerCase() === "w") {
        event.preventDefault();
        closeTab(activeId);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setContextMenu(null);
        void invoke("hide_window");
        return;
      }
      if (editing) return;
      if (event.key === "ArrowDown" || (event.ctrlKey && event.key.toLowerCase() === "j")) {
        event.preventDefault();
        selectOffset(1);
      } else if (event.key === "ArrowUp" || (event.ctrlKey && event.key.toLowerCase() === "k")) {
        event.preventDefault();
        selectOffset(-1);
      } else if (event.key === "ArrowRight" || event.key === "Enter") {
        event.preventDefault();
        void primaryAction(selectedItem);
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        goBack();
      } else if (event.key === " " && selectedItem && activeTab.mode !== "benchmark") {
        event.preventDefault();
        void invoke("toggle_quick_look", { path: selectedItem.path });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [activeId, activeTab.mode, closeTab, createTab, goBack, primaryAction, selectOffset, selectedItem]);

  const rowVirtualizer = useVirtualizer({
    count: activeTab.items.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 38,
    overscan: 14,
  });

  useEffect(() => {
    if (activeTab.items.length) rowVirtualizer.scrollToIndex(activeTab.selected, { align: "auto" });
  }, [activeTab.selected, activeTab.items.length, rowVirtualizer]);

  useEffect(() => {
    if (activeTab.mode !== "benchmark" || loggedBenchmarkTabs.has(activeTab.id)) return;
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        loggedBenchmarkTabs.add(activeTab.id);
        void invoke("log_frontend_event", {
          name: "benchmark_10k_rendered",
          fields: { item_count: 10_000, rendered_rows: rowVirtualizer.getVirtualItems().length },
        });
      });
    });
  }, [activeTab.id, activeTab.mode, rowVirtualizer]);

  const submitPath = (event: FormEvent) => {
    event.preventDefault();
    if (!activeTab.pathDraft.trim()) return;
    void loadPath(activeId, activeTab.pathDraft, true);
  };

  const openContextMenu = (event: MouseEvent, item: Item) => {
    event.preventDefault();
    updateTab(activeId, { selected: activeTab.items.indexOf(item) });
    setContextMenu({ x: event.clientX, y: event.clientY, item });
  };

  const contextAction = async (action: "open" | "copy" | "quicklook") => {
    if (!contextMenu || activeTab.mode === "benchmark") return;
    const item = contextMenu.item;
    setContextMenu(null);
    if (action === "open") await primaryAction(item);
    if (action === "copy") await invoke("copy_path", { path: item.path });
    if (action === "quicklook") await invoke("toggle_quick_look", { path: item.path });
  };

  return (
    <main className="app" onClick={() => setContextMenu(null)}>
      <header className="chrome" data-tauri-drag-region>
        <div className="tabs" data-tauri-drag-region>
          {tabs.map((tab) => (
            <button
              className={`tab ${tab.id === activeId ? "active" : ""}`}
              key={tab.id}
              onClick={(event) => {
                event.stopPropagation();
                setActiveId(tab.id);
              }}
            >
              <span>{tab.title}</span>
              <span
                className="tab-close"
                role="button"
                aria-label={`Close ${tab.title}`}
                onClick={(event) => {
                  event.stopPropagation();
                  closeTab(tab.id);
                }}
              >
                ×
              </span>
            </button>
          ))}
          <button className="icon-button" aria-label="New tab" onClick={createTab}>+</button>
          <button className="benchmark-button" onClick={createBenchmarkTab}>10k</button>
        </div>
        <form className="path-form" onSubmit={submitPath}>
          <button type="button" className="back-button" disabled={!activeTab.history.length} onClick={goBack}>‹</button>
          <input
            ref={inputRef}
            aria-label="Path Input"
            placeholder="Paste an absolute or ~ path"
            spellCheck={false}
            value={activeTab.pathDraft}
            onChange={(event) => updateTab(activeId, { pathDraft: event.target.value })}
            onKeyDown={(event: ReactKeyboardEvent) => {
              if (event.key === "Escape") inputRef.current?.blur();
            }}
          />
        </form>
      </header>

      <section className="list-shell" aria-busy={activeTab.loading}>
        <div className="list-header row-grid">
          <span>Name</span><span>Kind</span><span>Modified</span><span>Size</span>
        </div>
        <div className="list-scroll" ref={listRef} tabIndex={0}>
          {activeTab.loading && <div className="empty-state">Loading…</div>}
          {!activeTab.loading && activeTab.error && <div className="empty-state error">{activeTab.error}</div>}
          {!activeTab.loading && !activeTab.error && !activeTab.items.length && <div className="empty-state">No items</div>}
          <div className="virtual-space" style={{ height: rowVirtualizer.getTotalSize() }}>
            {rowVirtualizer.getVirtualItems().map((virtualRow) => {
              const item = activeTab.items[virtualRow.index];
              return (
                <div
                  className={`file-row row-grid ${virtualRow.index === activeTab.selected ? "selected" : ""}`}
                  key={item.path}
                  style={{ transform: `translateY(${virtualRow.start}px)` }}
                  onClick={(event) => {
                    event.stopPropagation();
                    updateTab(activeId, { selected: virtualRow.index });
                    listRef.current?.focus();
                  }}
                  onDoubleClick={() => void primaryAction(item)}
                  onContextMenu={(event) => openContextMenu(event, item)}
                >
                  <span className="name-cell"><span className="kind-icon">{item.is_dir ? "▸" : "·"}</span>{item.name}</span>
                  <span>{item.kind}</span>
                  <span>{formatDate(item.modified_ms)}</span>
                  <span className="size-cell">{formatSize(item.size)}</span>
                </div>
              );
            })}
          </div>
        </div>
      </section>

      <footer className="status">
        <span>{activeTab.location}</span>
        <span>{activeTab.loading ? "Loading" : `${activeTab.items.length.toLocaleString()} items`}</span>
      </footer>

      {contextMenu && (
        <div className="context-menu" style={{ left: contextMenu.x, top: contextMenu.y }} onClick={(event) => event.stopPropagation()}>
          <button onClick={() => void contextAction("open")}>{contextMenu.item.is_dir ? "Enter folder" : "Open"}</button>
          <button onClick={() => void contextAction("copy")}>Copy path</button>
          {!contextMenu.item.is_dir && <button onClick={() => void contextAction("quicklook")}>Quick Look</button>}
        </div>
      )}
    </main>
  );
}

export default App;
