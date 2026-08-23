import {
  useEffect,
  useRef,
  useState,
  type DragEvent,
  type ReactElement,
} from "react";

import { strings } from "../strings";
import { folderName, type Tab, type TabId } from "../tabs/model";
import type { Tabs } from "../tabs/useTabs";

function tabTitle(tab: Tab): string {
  if (tab.kind === "pinned") {
    return tab.customName ?? folderName(tab.anchorPath);
  }
  return tab.browse.location === ""
    ? strings.tabs.untitled
    : folderName(tab.browse.location);
}

interface MenuState {
  id: TabId;
  kind: Tab["kind"];
  x: number;
  y: number;
}

interface TabItemProps {
  tab: Tab;
  index: number;
  isActive: boolean;
  renaming: boolean;
  controller: Tabs;
  onOpenMenu: (menu: MenuState) => void;
  onDragStartTab: (id: TabId) => void;
  onDropTab: (targetIndex: number) => void;
  onCommitRename: (id: TabId, name: string) => void;
}

function TabItem({
  tab,
  index,
  isActive,
  renaming,
  controller,
  onOpenMenu,
  onDragStartTab,
  onDropTab,
  onCommitRename,
}: TabItemProps): ReactElement {
  const fill = isActive
    ? "bg-neutral-700 text-white"
    : "bg-neutral-800/50 text-neutral-300 hover:bg-neutral-800";

  return (
    <div
      role="tab"
      aria-selected={isActive}
      draggable={!renaming}
      onDragStart={(event: DragEvent<HTMLDivElement>) => {
        event.dataTransfer.setData("text/plain", tab.id);
        event.dataTransfer.effectAllowed = "move";
        onDragStartTab(tab.id);
      }}
      onDragOver={(event: DragEvent<HTMLDivElement>) => {
        event.preventDefault();
      }}
      onDrop={(event: DragEvent<HTMLDivElement>) => {
        event.preventDefault();
        event.stopPropagation();
        onDropTab(index);
      }}
      onClick={() => {
        controller.clickTab(tab.id);
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        onOpenMenu({
          id: tab.id,
          kind: tab.kind,
          x: event.clientX,
          y: event.clientY,
        });
      }}
      className={`group flex h-7 shrink basis-40 min-w-[120px] max-w-[200px] cursor-default items-center gap-1.5 rounded-t border-r border-neutral-800 px-2.5 ${fill}`}
    >
      <span
        aria-hidden="true"
        className={`h-1.5 w-1.5 shrink-0 rounded-full ${
          tab.kind === "pinned" ? "bg-amber-400" : "bg-transparent"
        }`}
      />
      {renaming ? (
        <input
          autoFocus
          defaultValue={tab.kind === "pinned" ? tabTitle(tab) : ""}
          aria-label={strings.tabs.renamePlaceholder}
          onClick={(event) => {
            event.stopPropagation();
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              onCommitRename(tab.id, event.currentTarget.value);
            } else if (event.key === "Escape") {
              onCommitRename(tab.id, tabTitle(tab));
            }
          }}
          onBlur={(event) => {
            onCommitRename(tab.id, event.currentTarget.value);
          }}
          className="min-w-0 flex-1 rounded bg-neutral-900 px-1 text-neutral-100 outline-none"
        />
      ) : (
        <span className="min-w-0 flex-1 truncate">{tabTitle(tab)}</span>
      )}
      {tab.kind === "temporary" && !renaming ? (
        <button
          type="button"
          aria-label={strings.tabs.menu.close}
          onClick={(event) => {
            event.stopPropagation();
            controller.removeTab(tab.id);
          }}
          className="shrink-0 rounded px-1 text-neutral-500 opacity-0 hover:text-neutral-100 group-hover:opacity-100"
        >
          ×
        </button>
      ) : null}
    </div>
  );
}

function TabContextMenu({
  menu,
  controller,
  onClose,
  onStartRename,
}: {
  menu: MenuState;
  controller: Tabs;
  onClose: () => void;
  onStartRename: (id: TabId) => void;
}): ReactElement {
  const items =
    menu.kind === "temporary"
      ? [
          {
            label: strings.tabs.menu.pin,
            run: () => {
              controller.pinTab(menu.id);
            },
          },
          {
            label: strings.tabs.menu.close,
            run: () => {
              controller.removeTab(menu.id);
            },
          },
          {
            label: strings.tabs.menu.copyLocation,
            run: () => {
              controller.copyLocation(menu.id);
            },
          },
        ]
      : [
          {
            label: strings.tabs.menu.rename,
            run: () => {
              onStartRename(menu.id);
            },
          },
          {
            label: strings.tabs.menu.unpin,
            run: () => {
              controller.unpinTab(menu.id);
            },
          },
          {
            label: strings.tabs.menu.remove,
            run: () => {
              controller.removeTab(menu.id);
            },
          },
          {
            label: strings.tabs.menu.copyLocation,
            run: () => {
              controller.copyLocation(menu.id);
            },
          },
        ];

  return (
    <div
      role="menu"
      style={{ left: menu.x, top: menu.y }}
      className="fixed z-50 min-w-[160px] rounded border border-neutral-700 bg-neutral-800 py-1 text-neutral-200 shadow-lg"
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          onClick={() => {
            item.run();
            onClose();
          }}
          className="block w-full px-3 py-1 text-left hover:bg-neutral-700"
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}

export function TabStrip({ controller }: { controller: Tabs }): ReactElement {
  const { state } = controller;
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [renamingId, setRenamingId] = useState<TabId | null>(null);
  const draggingRef = useRef<TabId | null>(null);

  // Dismiss the context menu on any outside interaction or Escape.
  useEffect(() => {
    if (menu === null) {
      return;
    }
    function close(): void {
      setMenu(null);
    }
    function onKey(event: KeyboardEvent): void {
      if (event.key === "Escape") {
        setMenu(null);
      }
    }
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  function dropOnto(targetIndex: number): void {
    const sourceId = draggingRef.current;
    draggingRef.current = null;
    if (sourceId === null) {
      return;
    }
    const target = state.tabs[targetIndex];
    if (target === undefined || target.id === sourceId) {
      return;
    }
    controller.dropOnGroup(sourceId, target.kind, targetIndex);
  }

  function dropOnGroupEnd(
    group: "pinned" | "temporary",
    targetIndex: number,
  ): void {
    const sourceId = draggingRef.current;
    draggingRef.current = null;
    if (sourceId === null) {
      return;
    }
    controller.dropOnGroup(sourceId, group, targetIndex);
  }

  function commitRename(id: TabId, name: string): void {
    controller.renameTab(id, name);
    setRenamingId(null);
  }

  const pinnedCountValue = state.tabs.filter(
    (tab) => tab.kind === "pinned",
  ).length;

  return (
    <div
      className="flex h-9 shrink-0 items-end gap-px overflow-x-auto border-b border-neutral-800 px-1 pt-1"
      aria-label={strings.zones.tabStrip}
    >
      <div
        aria-hidden="true"
        onDragOver={(event) => {
          event.preventDefault();
        }}
        onDrop={(event) => {
          event.preventDefault();
          dropOnGroupEnd("pinned", 0);
        }}
        className="h-5 w-1.5 shrink-0 self-center"
      />
      {state.tabs.map((tab, index) => (
        <div key={tab.id} className="contents">
          {index === pinnedCountValue && pinnedCountValue > 0 ? (
            <div
              aria-hidden="true"
              className="mx-1 h-5 w-px shrink-0 self-center bg-neutral-700"
            />
          ) : null}
          <TabItem
            tab={tab}
            index={index}
            isActive={tab.id === state.activeId}
            renaming={renamingId === tab.id}
            controller={controller}
            onOpenMenu={setMenu}
            onDragStartTab={(id) => {
              draggingRef.current = id;
            }}
            onDropTab={dropOnto}
            onCommitRename={commitRename}
          />
        </div>
      ))}
      <div
        className="flex-1 self-stretch"
        onDragOver={(event) => {
          event.preventDefault();
        }}
        onDrop={(event) => {
          event.preventDefault();
          dropOnGroupEnd("temporary", state.tabs.length);
        }}
      />
      <button
        type="button"
        aria-label={strings.tabs.newTab}
        onClick={() => {
          controller.newTemporaryTab();
        }}
        className="mb-0.5 shrink-0 rounded px-2 py-0.5 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
      >
        +
      </button>
      {menu !== null ? (
        <TabContextMenu
          menu={menu}
          controller={controller}
          onClose={() => {
            setMenu(null);
          }}
          onStartRename={(id) => {
            setRenamingId(id);
            setMenu(null);
          }}
        />
      ) : null}
    </div>
  );
}
