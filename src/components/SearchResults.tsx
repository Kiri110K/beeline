import { useEffect, useRef, type ReactElement } from "react";
import { match } from "ts-pattern";

import type { SearchHit } from "../search/ipc";
import { displayedHits, type ResultsView } from "../search/state";
import { strings } from "../strings";
import { parentPath } from "../tabs/model";
import { FileGlyph, FolderGlyph } from "./icons";
import { ROW_HEIGHT } from "./layout";

interface SearchResultsProps {
  results: ResultsView;
  focusedIndex: number;
  onReveal: (index: number) => void;
}

function tierHint(tier: SearchHit["tier"]): string | null {
  return match(tier)
    .with("normal", () => null)
    .with("hidden", () => strings.search.tier.hidden)
    .with("junk", () => strings.search.tier.junk)
    .exhaustive();
}

function SearchRow({
  hit,
  isFocused,
  onReveal,
}: {
  hit: SearchHit;
  isFocused: boolean;
  onReveal: () => void;
}): ReactElement {
  // Hidden and Junk hits are subtly dimmed (§6); a single click Reveals (§5).
  const dim = hit.tier !== "normal";
  const hint = tierHint(hit.tier);
  const fill = isFocused
    ? "bg-blue-600 text-white"
    : "text-neutral-200 hover:bg-neutral-800";
  const subtle = isFocused ? "text-blue-100" : "text-neutral-500";
  return (
    <div
      role="row"
      aria-selected={isFocused}
      onClick={onReveal}
      style={{ height: ROW_HEIGHT }}
      className={`flex items-center gap-2 px-3 ${fill} ${dim ? "opacity-60" : ""}`}
    >
      <span className={`shrink-0 ${subtle}`}>
        {hit.isDirectory ? <FolderGlyph /> : <FileGlyph />}
      </span>
      <span className="shrink-0 truncate">{hit.name}</span>
      <span className={`min-w-0 flex-1 truncate text-xs ${subtle}`}>
        {parentPath(hit.path)}
      </span>
      {hint !== null ? (
        <span className={`shrink-0 text-xs uppercase tracking-wide ${subtle}`}>
          {hint}
        </span>
      ) : null}
    </div>
  );
}

// The Search Results overlay (SPEC §5, §6): floats over the Browse list while
// Search Mode is active. It never touches the current Location, selection, or
// scroll — those live untouched underneath and survive closing the overlay.
// Failures and emptiness are row-state lines, never banners or raw error text.
export function SearchResults({
  results,
  focusedIndex,
  onReveal,
}: SearchResultsProps): ReactElement | null {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const hits = displayedHits(results);

  // Keep the Focused result in view as keyboard navigation moves it.
  useEffect(() => {
    const element = scrollRef.current;
    if (element === null) {
      return;
    }
    const top = focusedIndex * ROW_HEIGHT;
    const bottom = top + ROW_HEIGHT;
    if (top < element.scrollTop) {
      element.scrollTop = top;
    } else if (bottom > element.scrollTop + element.clientHeight) {
      element.scrollTop = bottom - element.clientHeight;
    }
  }, [focusedIndex]);

  if (hits.length === 0) {
    // No rows: one row-state line, or nothing (idle / not-yet-slow) so the Browse
    // list shows through (e.g. a fresh Tab's empty query).
    const line = match(results)
      .with({ status: "failed" }, () => strings.search.failed)
      .with({ status: "done" }, () => strings.search.empty)
      .with({ status: "pending" }, ({ slow }) =>
        slow ? strings.search.searching : null,
      )
      .with({ status: "idle" }, () => null)
      .exhaustive();
    if (line === null) {
      return null;
    }
    return (
      <div className="absolute inset-0 z-10 bg-neutral-900">
        <div className="px-3 py-2 text-neutral-500">{line}</div>
      </div>
    );
  }

  return (
    <div ref={scrollRef} className="absolute inset-0 z-10 overflow-auto bg-neutral-900">
      {hits.map((hit, index) => (
        <SearchRow
          key={hit.path}
          hit={hit}
          isFocused={index === focusedIndex}
          onReveal={() => {
            onReveal(index);
          }}
        />
      ))}
    </div>
  );
}
