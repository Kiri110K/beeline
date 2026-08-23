import { memo, type ReactElement } from "react";

import type { SelectMode } from "../browse/state";
import { formatModified, formatSize } from "../location/format";
import type { Item } from "../location/schema";
import { FileGlyph, FolderGlyph } from "./icons";
import { GRID_COLS, ROW_HEIGHT } from "./layout";

interface FileRowProps {
  item: Item;
  index: number;
  isFocused: boolean;
  isSelected: boolean;
  onSelect: (index: number, mode: SelectMode) => void;
  onActivate: (item: Item) => void;
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

// Memoized on (isFocused, isSelected) booleans plus stable callbacks, so a
// selection change re-renders only the rows whose own flags flip — never the
// whole list (§10 keystroke budget).
export const FileRow = memo(function FileRow({
  item,
  index,
  isFocused,
  isSelected,
  onSelect,
  onActivate,
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
        onSelect(index, selectModeOf(event));
      }}
      onDoubleClick={() => {
        onActivate(item);
      }}
      style={{ top: index * ROW_HEIGHT, height: ROW_HEIGHT }}
      className={`${GRID_COLS} absolute inset-x-0 ${fill}`}
    >
      <span className="flex min-w-0 items-center gap-2">
        <span className={`shrink-0 ${subtle}`}>
          {item.isDirectory ? <FolderGlyph /> : <FileGlyph />}
        </span>
        <span className={`truncate ${item.isHidden ? "opacity-60" : ""}`}>
          {item.name}
        </span>
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
