import type { ReactElement } from "react";

import { FileTable } from "./components/FileTable";
import { TabStrip } from "./components/TabStrip";
import { strings } from "./strings";
import { useTabs } from "./tabs/useTabs";

// SPEC §3 layout, top to bottom: Tab strip, Navigation Input, dense file table,
// reserved Preview Panel column, Status Strip zone.
export default function App(): ReactElement {
  const tabs = useTabs();
  const browse = tabs.activeBrowse;
  return (
    <div className="flex h-screen flex-col bg-neutral-900 text-[13px] text-neutral-100">
      <TabStrip controller={tabs} />
      <div className="shrink-0 border-b border-neutral-800 p-1.5">
        <input
          type="text"
          value={browse.location}
          disabled
          readOnly
          aria-label={strings.navigation.inputLabel}
          className="w-full rounded bg-neutral-800 px-2 py-1 text-neutral-300 outline-none"
        />
      </div>
      <div className="flex min-h-0 flex-1">
        <FileTable
          load={browse.load}
          location={browse.location}
          focusedIndex={browse.focusedIndex}
          selected={browse.selected}
          pendingScrollTop={browse.pendingScrollTop}
          scrollGeneration={browse.scrollGeneration}
          onSelect={tabs.select}
          onActivate={tabs.activateItem}
          onScrollTop={tabs.onScrollTop}
        />
        <div
          className="w-[300px] shrink-0 border-l border-neutral-800"
          aria-label={strings.zones.previewPanel}
        />
      </div>
      <div
        className="h-6 shrink-0 border-t border-neutral-800"
        aria-label={strings.zones.statusStrip}
      />
    </div>
  );
}
