import { useCallback, useEffect, useRef, useState } from "react";
import { type ResultAsync } from "neverthrow";

import { resolveInstalledBundle } from "../operations/ipc";
import {
  EDITOR_BUNDLE_IDS,
  seededSlotUpdate,
  TERMINAL_BUNDLE_IDS,
} from "../operations/settings";
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

  // First-run auto-seeding of the Terminal/Editor slots (SPEC §8, §12), run exactly once after
  // the first settings load lands. Detect a candidate for each empty slot from its priority
  // list, resolve both before persisting, then write at most one combined update — and only for
  // slots still empty at save time, so a slot the user (or another write) filled during
  // detection is never overwritten. The `seededRef` guard means the `settings-changed` re-pull
  // this save triggers never re-seeds, so there is no effect loop.
  const seededRef = useRef(false);
  const seedApplicationSlots = useCallback(
    (loaded: Settings): void => {
      if (loaded.terminalBundleId !== null && loaded.editorBundleId !== null) {
        return; // both slots already configured — nothing to detect
      }
      const detect = (
        empty: boolean,
        ids: readonly string[],
      ): Promise<string | null> =>
        empty
          ? resolveInstalledBundle([...ids]).match(
              (value) => value,
              (error) => {
                reportShellError(error);
                return null;
              },
            )
          : Promise.resolve<string | null>(null);
      void Promise.all([
        detect(loaded.terminalBundleId === null, TERMINAL_BUNDLE_IDS),
        detect(loaded.editorBundleId === null, EDITOR_BUNDLE_IDS),
      ]).then(([terminal, editor]) => {
        // Read the freshest settings, not `loaded`: a user or another write may have filled a
        // slot while detection was in flight, and `seededSlotUpdate` must preserve it.
        const base = settingsRef.current;
        const update = seededSlotUpdate(
          {
            terminalBundleId: base.terminalBundleId,
            editorBundleId: base.editorBundleId,
          },
          { terminalBundleId: terminal, editorBundleId: editor },
        );
        if (update === null) {
          return; // no candidate installed for any empty slot
        }
        const next: Settings = { ...base, ...update };
        void save(next).match(() => undefined, reportShellError);
      }, reportShellError);
    },
    [save],
  );

  const pull = useCallback((): void => {
    void getSettings().match((loaded) => {
      settingsRef.current = loaded;
      setSettingsState(loaded);
      if (!seededRef.current) {
        seededRef.current = true;
        seedApplicationSlots(loaded);
      }
    }, reportShellError);
  }, [seedApplicationSlots]);

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

  return { settings, save };
}
