import type { ReactElement } from "react";

import { strings } from "../strings";
import type { Tabs } from "../tabs/useTabs";

// The always-on Delete Permanently confirm (SPEC §8): a small in-app confirm bar, never a
// system dialog and never a "don't ask again". Enter confirms, Escape cancels — both are
// wired in the global keyboard handler; the buttons mirror them for the mouse.
export function ConfirmBar({ controller }: { controller: Tabs }): ReactElement | null {
  const { ops, confirmDelete, cancelDelete } = controller;
  const confirm = ops.confirm;
  if (confirm === null) {
    return null;
  }
  return (
    <div
      role="alertdialog"
      aria-label={strings.operations.confirm.confirm}
      className="flex shrink-0 items-center gap-3 border-t border-red-900/60 bg-red-950/40 px-3 py-1.5 text-xs text-red-100"
    >
      <span className="min-w-0 flex-1 truncate">
        {strings.operations.confirm.message(confirm.paths.length)}
      </span>
      <button
        type="button"
        onClick={() => {
          cancelDelete();
        }}
        className="shrink-0 rounded px-2 py-0.5 text-neutral-300 hover:bg-neutral-800"
      >
        {strings.operations.confirm.cancel}
      </button>
      <button
        type="button"
        onClick={() => {
          confirmDelete();
        }}
        className="shrink-0 rounded bg-red-600 px-2 py-0.5 font-medium text-white hover:bg-red-500"
      >
        {strings.operations.confirm.confirm}
      </button>
    </div>
  );
}
