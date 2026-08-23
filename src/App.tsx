import type { ReactElement } from "react";

import { useBrowse } from "./browse/useBrowse";
import { FileTable } from "./components/FileTable";
import { strings } from "./strings";

// SPEC §3 layout skeleton, top to bottom: Tab strip zone, Navigation Input,
// dense file table, reserved Preview Panel column, Status Strip zone.
export default function App(): ReactElement {
  const { state, focusIndex, activate } = useBrowse();
  return (
    <div className="flex h-screen flex-col bg-neutral-900 text-[13px] text-neutral-100">
      <div
        className="h-9 shrink-0 border-b border-neutral-800"
        aria-label={strings.zones.tabStrip}
      />
      <div className="shrink-0 border-b border-neutral-800 p-1.5">
        <input
          type="text"
          value={state.location}
          disabled
          readOnly
          aria-label={strings.navigation.inputLabel}
          className="w-full rounded bg-neutral-800 px-2 py-1 text-neutral-300 outline-none"
        />
      </div>
      <div className="flex min-h-0 flex-1">
        <FileTable
          load={state.load}
          location={state.location}
          focusedIndex={state.focusedIndex}
          onFocusIndex={focusIndex}
          onActivate={activate}
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
