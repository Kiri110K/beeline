import { memo, type ReactElement } from "react";

import type { SelectMode } from "../browse/state";
import { formatModified, formatSize } from "../location/format";
import type { MenuVia } from "../operations/state";
import type { Item } from "../location/schema";
import { strings } from "../strings";
import { FileGlyph, FolderGlyph } from "./icons";
import { GRID_COLS, ROW_HEIGHT } from "./layout";

interface FileRowProps {
  item: Item;
  index: number;
  isFocused: boolean;
  isSelected: boolean;
  // Inline rename (§8): true only for the row being renamed; `renameError` carries the
  // engine's collision message shown under the field (null until one occurs).
  renaming: boolean;
  renameError: string | null;
  onSelect: (index: number, mode: SelectMode) => void;
  onActivate: (item: Item) => void;
  onOpenMenu: (index: number, via: MenuVia, x: number, y: number) => void;
  onCommitRename: (name: string) => void;
  onCancelRename: () => void;
}

function selectModeOf(event: {
  shiftKey: boolean;
  metaKey: boolean;
}): SelectMode {
  if (event.shiftKey) {
    return "range";
  }
  if (event.metaKey) {
    return "toggle";
  }
  return "plain";
}

// Memoized on (isFocused, isSelected, renaming, renameError) plus stable callbacks, so a
// selection change re-renders only the rows whose own flags flip — never the whole list
// (§10 keystroke budget).
export const FileRow = memo(function FileRow({
  item,
  index,
  isFocused,
  isSelected,
  renaming,
  renameError,
  onSelect,
  onActivate,
  onOpenMenu,
  onCommitRename,
  onCancelRename,
}: FileRowProps): ReactElement {
  // Focused wins over selected when a row is both; a selected-only row gets a
  // lighter fill.
  const fill = isFocused
    ? "bg-blue-600 text-white"
    : isSelected
      ? "bg-blue-600/25 text-white"
      : "text-neutral-200 hover:bg-neutral-800";
  const subtle = isFocused
    ? "text-blue-100"
    : isSelected
      ? "text-neutral-300"
      : "text-neutral-400";
  return (
    <div
      role="row"
      aria-selected={isSelected}
      onClick={(event) => {
        if (!renaming) {
          onSelect(index, selectModeOf(event));
        }
      }}
      onDoubleClick={() => {
        if (!renaming) {
          onActivate(item);
        }
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        onOpenMenu(index, "context", event.clientX, event.clientY);
      }}
      style={{ top: index * ROW_HEIGHT, height: ROW_HEIGHT }}
      className={`group ${GRID_COLS} absolute inset-x-0 ${fill}`}
    >
      <span className="flex min-w-0 items-center gap-2">
        <span className={`shrink-0 ${subtle}`}>
          {item.isDirectory ? <FolderGlyph /> : <FileGlyph />}
        </span>
        {renaming ? (
          <span className="relative flex min-w-0 flex-1 items-center">
            <input
              autoFocus
              defaultValue={item.name}
              aria-label={strings.operations.renamePlaceholder}
              spellCheck={false}
              autoComplete="off"
              onClick={(event) => {
                event.stopPropagation();
              }}
              onKeyDown={(event) => {
                event.stopPropagation();
                if (event.key === "Enter") {
                  onCommitRename(event.currentTarget.value);
                } else if (event.key === "Escape") {
                  onCancelRename();
                }
              }}
              onBlur={(event) => {
                onCommitRename(event.currentTarget.value);
              }}
              className="min-w-0 flex-1 rounded bg-neutral-900 px-1 text-neutral-100 outline-none"
            />
            {renameError !== null ? (
              <span className="absolute left-0 top-full z-10 mt-0.5 rounded bg-red-950 px-1.5 py-0.5 text-xs text-red-200 shadow">
                {renameError}
              </span>
            ) : null}
          </span>
        ) : (
          <span className={`truncate ${item.isHidden ? "opacity-60" : ""}`}>
            {item.name}
          </span>
        )}
        {isFocused && !renaming ? (
          <button
            type="button"
            aria-label={strings.operations.rowMenuLabel}
            onClick={(event) => {
              event.stopPropagation();
              const rect = event.currentTarget.getBoundingClientRect();
              onOpenMenu(index, "row_button", rect.right, rect.bottom);
            }}
            className="ml-auto shrink-0 rounded px-1 text-blue-100 hover:bg-white/20"
          >
            …
          </button>
        ) : null}
      </span>
      <span className={`truncate ${subtle}`}>{item.kind}</span>
      <span className={`truncate ${subtle}`}>
        {formatModified(item.modifiedMs)}
      </span>
      <span className={`text-right tabular-nums ${subtle}`}>
        {formatSize(item.sizeBytes)}
      </span>
    </div>
  );
});
