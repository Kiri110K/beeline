import { type KeyboardEvent, type ReactElement } from "react";

import { strings } from "../strings";
import type { Tabs } from "../tabs/useTabs";

// The full-width Navigation Input (SPEC §3, §5): a real editable field whose text
// is always a Search Query through the one ranker. It is the DOM focus holder in
// Search Mode, so it owns the result-navigation keys; the global Browse-mode
// handler stands down while it is focused. Left/Right are never intercepted —
// they edit the query (point 2) — and typing itself is never blocked (§10).
export function NavigationInput({
  controller,
}: {
  controller: Tabs;
}): ReactElement {
  const {
    activeSearch,
    setInputEl,
    activateSearch,
    deactivateSearch,
    changeQuery,
    focusResultDelta,
    revealFocused,
  } = controller;

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>): void {
    if (event.key === "Escape") {
      // Close Search Results (Escape order, §5); do not let the window hide.
      event.preventDefault();
      event.stopPropagation();
      deactivateSearch();
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusResultDelta(1);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      focusResultDelta(-1);
      return;
    }
    if (event.ctrlKey && (event.key === "j" || event.key === "J")) {
      event.preventDefault();
      focusResultDelta(1);
      return;
    }
    if (event.ctrlKey && (event.key === "k" || event.key === "K")) {
      event.preventDefault();
      focusResultDelta(-1);
      return;
    }
    if (event.key === "Enter") {
      // Enter Reveals the Focused result — never an external open (§6, point 7).
      event.preventDefault();
      revealFocused();
      return;
    }
  }

  return (
    <div className="shrink-0 border-b border-neutral-800 p-1.5">
      <input
        ref={setInputEl}
        type="text"
        value={activeSearch.query}
        placeholder={strings.navigation.placeholder}
        aria-label={strings.navigation.searchLabel}
        spellCheck={false}
        autoComplete="off"
        onFocus={() => {
          if (activeSearch.mode === "browse") {
            activateSearch();
          }
        }}
        onChange={(event) => {
          changeQuery(event.currentTarget.value);
        }}
        onKeyDown={onKeyDown}
        className="w-full rounded bg-neutral-800 px-2 py-1 text-neutral-100 outline-none placeholder:text-neutral-500"
      />
    </div>
  );
}
