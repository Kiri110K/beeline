import { useEffect, type ReactElement } from "react";
import { type ResultAsync } from "neverthrow";

import { recordTelemetry, reportShellError, type ShellError } from "../shell";
import { strings } from "../strings";
import { enableLoginItem, openFullDiskAccessSettings, type SetSettingsOutcome } from "./ipc";
import type { Settings } from "./schema";

// The one-time first-run guidance (SPEC §13): a non-blocking, dismissible panel that requests
// Full Disk Access and offers Login Item enrolment. It is shown until dismissed (the persisted
// `firstRunDismissed` flag), and never gates anything — degraded operation without either
// grant still works (§13). Rendered by App only while `!settings.firstRunDismissed`.
export function FirstRunBanner({
  settings,
  save,
}: {
  settings: Settings;
  save: (next: Settings) => ResultAsync<SetSettingsOutcome, ShellError>;
}): ReactElement {
  useEffect(() => {
    void recordTelemetry("first_run_shown", {}).match(
      () => undefined,
      reportShellError,
    );
  }, []);

  const requestFullDiskAccess = (): void => {
    void openFullDiskAccessSettings().match(() => undefined, reportShellError);
  };

  const enrolLoginItem = (): void => {
    void enableLoginItem().match(() => undefined, reportShellError);
  };

  const dismiss = (): void => {
    void recordTelemetry("first_run_dismissed", {}).match(
      () => undefined,
      reportShellError,
    );
    void save({ ...settings, firstRunDismissed: true }).match(
      () => undefined,
      reportShellError,
    );
  };

  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-8 z-40 flex justify-center">
      <div className="pointer-events-auto mx-4 max-w-lg rounded-lg border border-neutral-700 bg-neutral-800 p-4 shadow-xl">
        <div className="mb-1 text-sm font-medium text-neutral-100">
          {strings.firstRun.title}
        </div>
        <p className="mb-3 text-[12px] leading-snug text-neutral-400">
          {strings.firstRun.body}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={requestFullDiskAccess}
            className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500"
          >
            {strings.firstRun.fullDiskAccess}
          </button>
          <button
            type="button"
            onClick={enrolLoginItem}
            className="rounded border border-neutral-600 px-3 py-1 text-neutral-200 hover:bg-neutral-700"
          >
            {strings.firstRun.loginItem}
          </button>
          <button
            type="button"
            onClick={dismiss}
            className="ml-auto rounded px-3 py-1 text-neutral-400 hover:text-neutral-200"
          >
            {strings.firstRun.dismiss}
          </button>
        </div>
      </div>
    </div>
  );
}
