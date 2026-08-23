import { useCallback, useEffect, useRef, useState } from "react";
import { type ResultAsync } from "neverthrow";

import { recordTelemetry, reportShellError, type ShellError } from "../shell";
import {
  getSettings,
  setSettings,
  subscribeSettingsChanged,
  type SetSettingsOutcome,
} from "./ipc";
import { DEFAULT_SETTINGS, type Settings } from "./schema";

// The one settings store the app reads (SPEC §12): the live settings plus a `save` that
// persists them, records `settings_changed {field}` telemetry, and lets the backend apply
// the live consumers. Loaded once on mount and re-pulled on `beeline://settings-changed`
// (which the backend emits on every write), so a change from the Settings view or the
// first-run flow reaches every consumer.
export interface SettingsStore {
  settings: Settings;
  save: (next: Settings) => ResultAsync<SetSettingsOutcome, ShellError>;
}

// The user-facing fields whose change is worth a `settings_changed {field}` event (no
// values, SPEC telemetry point). `firstRunDismissed` is internal (§13) and excluded — its
// own `first_run_dismissed` event covers it.
const TRACKED_FIELDS = [
  "globalShortcut",
  "defaultEntryPoint",
  "temporaryTabLifetime",
  "primaryActionDirectory",
  "primaryActionFile",
  "afterAction",
  "previewPanelVisible",
  "terminalBundleId",
  "editorBundleId",
  "aliases",
  "junkPatterns",
] as const;

function fireChangedTelemetry(prev: Settings, next: Settings): void {
  for (const field of TRACKED_FIELDS) {
    if (JSON.stringify(prev[field]) !== JSON.stringify(next[field])) {
      void recordTelemetry("settings_changed", { field }).match(
        () => undefined,
        reportShellError,
      );
    }
  }
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
      fireChangedTelemetry(settingsRef.current, next);
      // Reflect immediately; the settings-changed re-pull reconciles anything the backend
      // adjusted (e.g. a rejected shortcut rolled back to the previous one).
      settingsRef.current = next;
      setSettingsState(next);
      return setSettings(next);
    },
    [],
  );

  return { settings, save };
}
