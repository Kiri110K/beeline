import { useCallback, useEffect, useRef, useState } from "react";
import { type ResultAsync } from "neverthrow";

import { recordTelemetry, reportShellError, type ShellError } from "../shell";
import {
  getSettings,
  setSettings,
  subscribeSettingsChanged,
  type SetSettingsOutcome,
} from "./ipc";
import { changedTrackedFields } from "./saveTelemetry";
import { DEFAULT_SETTINGS, type Settings } from "./schema";

// The one settings store the app reads (SPEC §12): the live settings plus a `save` that
// persists them and records `settings_changed {field}` telemetry. The store never applies a
// draft optimistically — the live settings only ever move forward to what the backend
// confirms. It is loaded once on mount and re-pulled on `beeline://settings-changed` (which
// the backend emits on every successful write), so a confirmed change from the Settings view
// or the first-run flow reaches every consumer, and a failed write leaves them untouched.
export interface SettingsStore {
  settings: Settings;
  save: (next: Settings) => ResultAsync<SetSettingsOutcome, ShellError>;
}

export function useSettingsStore(): SettingsStore {
  const [settings, setSettingsState] = useState<Settings>(DEFAULT_SETTINGS);
  const settingsRef = useRef(settings);
  useEffect(() => {
    settingsRef.current = settings;
  }, [settings]);

  const pull = useCallback((): void => {
    void getSettings().match((loaded) => {
      settingsRef.current = loaded;
      setSettingsState(loaded);
    }, reportShellError);
  }, []);

  useEffect(() => {
    pull();
    let unlisten: (() => void) | null = null;
    void subscribeSettingsChanged(pull).match((fn) => {
      unlisten = fn;
    }, reportShellError);
    return () => {
      if (unlisten !== null) {
        unlisten();
      }
    };
  }, [pull]);

  const save = useCallback(
    (next: Settings): ResultAsync<SetSettingsOutcome, ShellError> => {
      // No optimistic write: the draft reaches live consumers only through the
      // `settings-changed` re-pull the backend emits on success, so a failed write leaves
      // every consumer on the last confirmed settings and an older failure can never roll
      // back a newer success — the store only advances to what `get_settings` returns.
      const prev = settingsRef.current;
      return setSettings(next).map((outcome) => {
        // Telemetry fires only for a landed change, and never for a shortcut the backend
        // rejected and rolled back to the previous one (SPEC §2).
        for (const field of changedTrackedFields(prev, next, outcome)) {
          void recordTelemetry("settings_changed", { field }).match(
            () => undefined,
            reportShellError,
          );
        }
        return outcome;
      });
    },
    [],
  );

  return { settings, save };
}
