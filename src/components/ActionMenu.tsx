import type { ReactElement } from "react";

import type { Tabs } from "../tabs/useTabs";

// Where a keyboard-opened menu (Cmd+K, no anchor point) sits: below the Navigation Input,
// inset from the left. Row-anchored menus (`…` / context click) use their own x/y.
const KEYBOARD_MENU_LEFT = 12;
const KEYBOARD_MENU_TOP = 76;

// The one Action Menu (SPEC §5), reachable three ways. It is purely presentational: the
// controller derives the rows from the live selection and owns the keyboard (Up/Down,
// Enter, Escape); this renders those rows, highlights the keyboard-focused one, and closes
// on an outside click. Not a toast, not a banner — a transient list (§8 has no toasts).
export function ActionMenu({ controller }: { controller: Tabs }): ReactElement | null {
  const { ops, menuItems, focusMenuItem, runMenuItem, closeActionMenu } = controller;
  const menu = ops.menu;
  if (menu === null) {
    return null;
  }
  const left = menu.x ?? KEYBOARD_MENU_LEFT;
  const top = menu.y ?? KEYBOARD_MENU_TOP;

  return (
    <div
      className="fixed inset-0 z-50"
      onPointerDown={() => {
        closeActionMenu();
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        closeActionMenu();
      }}
    >
      <div
        role="menu"
        style={{ left, top }}
        onPointerDown={(event) => {
          event.stopPropagation();
        }}
        className="fixed max-h-[70vh] min-w-[200px] overflow-auto rounded border border-neutral-700 bg-neutral-800 py-1 text-neutral-200 shadow-lg"
      >
        {menuItems.map((item, index) => {
          const focused = index === menu.focusedIndex;
          const base = "block w-full px-3 py-1 text-left";
          const state = item.disabled
            ? "text-neutral-500"
            : focused
              ? "bg-blue-600 text-white"
              : "hover:bg-neutral-700";
          return (
            <button
              key={item.id}
              type="button"
              role="menuitem"
              disabled={item.disabled}
              onPointerMove={() => {
                if (!item.disabled && !focused) {
                  focusMenuItem(index);
                }
              }}
              onClick={() => {
                runMenuItem(item);
              }}
              className={`${base} ${state}`}
            >
              {item.label}
            </button>
          );
        })}
      </div>
    </div>
  );
}
