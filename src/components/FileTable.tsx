import { useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { match } from "ts-pattern";

import type { LoadState, SelectMode } from "../browse/state";
import type { Item, ListErrorPayload } from "../location/schema";
import { strings } from "../strings";
import { FileRow } from "./FileRow";
import { GRID_COLS, OVERSCAN, ROW_HEIGHT } from "./layout";

interface FileTableProps {
  load: LoadState;
  location: string;
  focusedIndex: number;
  selected: ReadonlySet<number>;
  pendingScrollTop: number;
  scrollGeneration: number;
  onSelect: (index: number, mode: SelectMode) => void;
  onActivate: (item: Item) => void;
  onScrollTop: (top: number) => void;
}

function errorText(error: ListErrorPayload, location: string): string {
  return match(error)
    .with({ code: "not-found" }, () => strings.rowState.notFound(location))
    .with({ code: "not-a-directory" }, () => strings.rowState.noAccess(location))
    .with({ code: "permission-denied" }, () =>
      strings.rowState.noAccess(location),
    )
    .with({ code: "io" }, () => strings.rowState.noAccess(location))
    .exhaustive();
}

function RowStateLine({ text }: { text: string }): ReactElement {
  return <div className="px-3 py-2 text-neutral-500">{text}</div>;
}

export function FileTable({
  load,
  location,
  focusedIndex,
  selected,
  pendingScrollTop,
  scrollGeneration,
  onSelect,
  onActivate,
  onScrollTop,
}: FileTableProps): ReactElement {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(0);

  useEffect(() => {
    const element = scrollRef.current;
    if (element === null) {
      return;
    }
    const observer = new ResizeObserver(() => {
      setViewportHeight(element.clientHeight);
    });
    observer.observe(element);
    setViewportHeight(element.clientHeight);
    return () => {
      observer.disconnect();
    };
  }, []);

  // Keep the Focused Item in view on keyboard moves and after each listing.
  useEffect(() => {
    const element = scrollRef.current;
    if (element === null || load.status !== "ready") {
      return;
    }
    const top = focusedIndex * ROW_HEIGHT;
    const bottom = top + ROW_HEIGHT;
    if (top < element.scrollTop) {
      element.scrollTop = top;
    } else if (bottom > element.scrollTop + element.clientHeight) {
      element.scrollTop = bottom - element.clientHeight;
    }
  }, [focusedIndex, load.status]);

  // Restore the saved scroll offset once per landed listing (history back/forward
  // and fresh entries). Runs after the focus-into-view effect so it wins.
  useEffect(() => {
    const element = scrollRef.current;
    if (element === null) {
      return;
    }
    element.scrollTop = pendingScrollTop;
    setScrollTop(pendingScrollTop);
    onScrollTop(pendingScrollTop);
    // Keyed on the generation so a same-value offset still re-applies.
  }, [scrollGeneration, pendingScrollTop, onScrollTop]);

  const content: ReactNode = match(load)
    .with({ status: "idle" }, () => null)
    .with({ status: "loading" }, () => null)
    .with({ status: "error" }, ({ error }) => (
      <RowStateLine text={errorText(error, location)} />
    ))
    .with({ status: "ready" }, ({ items }) => {
      if (items.length === 0) {
        return <RowStateLine text={strings.rowState.empty} />;
      }
      const count = items.length;
      const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
      const last = Math.min(
        count,
        Math.ceil((scrollTop + viewportHeight) / ROW_HEIGHT) + OVERSCAN,
      );
      return (
        <div className="relative" style={{ height: count * ROW_HEIGHT }}>
          {items.slice(first, last).map((item, offset) => {
            const index = first + offset;
            return (
              <FileRow
                key={item.path}
                item={item}
                index={index}
                isFocused={index === focusedIndex}
                isSelected={selected.has(index)}
                onSelect={onSelect}
                onActivate={onActivate}
              />
            );
          })}
        </div>
      );
    })
    .exhaustive();

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div
        className={`${GRID_COLS} shrink-0 border-b border-neutral-800 py-1 text-xs font-medium uppercase tracking-wide text-neutral-500`}
      >
        <span>{strings.columns.name}</span>
        <span>{strings.columns.kind}</span>
        <span>{strings.columns.modified}</span>
        <span className="text-right">{strings.columns.size}</span>
      </div>
      <div
        ref={scrollRef}
        onScroll={() => {
          const element = scrollRef.current;
          if (element !== null) {
            setScrollTop(element.scrollTop);
            onScrollTop(element.scrollTop);
          }
        }}
        className="min-h-0 flex-1 overflow-auto"
      >
        {content}
      </div>
    </div>
  );
}
