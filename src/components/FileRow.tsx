import { memo, type ReactElement } from "react";

import { formatModified, formatSize } from "../location/format";
import type { Item } from "../location/schema";
import { FileGlyph, FolderGlyph } from "./icons";
import { GRID_COLS, ROW_HEIGHT } from "./layout";

interface FileRowProps {
  item: Item;
  index: number;
  isFocused: boolean;
  onFocusIndex: (index: number) => void;
  onActivate: (item: Item) => void;
}

// Memoized on stable props (item identity, stable callbacks) so a focus move
// only re-renders the two rows whose `isFocused` flips.
export const FileRow = memo(function FileRow({
  item,
  index,
  isFocused,
  onFocusIndex,
  onActivate,
}: FileRowProps): ReactElement {
  const subtle = isFocused ? "text-blue-100" : "text-neutral-400";
  return (
    <div
      role="row"
      aria-selected={isFocused}
      onClick={() => {
        onFocusIndex(index);
      }}
      onDoubleClick={() => {
        onActivate(item);
      }}
      style={{ top: index * ROW_HEIGHT, height: ROW_HEIGHT }}
      className={`${GRID_COLS} absolute inset-x-0 ${
        isFocused ? "bg-blue-600 text-white" : "text-neutral-200 hover:bg-neutral-800"
      }`}
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
