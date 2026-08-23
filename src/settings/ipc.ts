import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";
import { settingsSchema, type Settings } from "./schema";

const SETTINGS_CHANGED_EVENT = "beeline://settings-changed";

// The deep link that opens System Settings › Privacy & Security › Full Disk Access (SPEC
// §13). macOS resolves this scheme; no grant is required to open it.
const FULL_DISK_ACCESS_URL =
  "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

const unitSchema = z.null();
const unlistenSchema = z.custom<UnlistenFn>(
  (value) => typeof value === "function",
);

// The result of a settings write (SPEC §2, §12): the backend reports whether the new global
// shortcut could be registered. `false` means it kept the previous shortcut.
const setSettingsOutcomeSchema = z
  .object({ shortcutRegistered: z.boolean() })
  .strict();
export type SetSettingsOutcome = z.infer<typeof setSettingsOutcomeSchema>;

// Read the whole settings surface (SPEC §12), parsed at the boundary.
export function getSettings(): ResultAsync<Settings, ShellError> {
  return fromTauri("get_settings", settingsSchema, () => invoke("get_settings"));
}

// Persist the whole settings surface and apply its live consumers (SPEC §12). The backend
// re-registers the shortcut, refreshes the Name Index, and emits `settings-changed`.
export function setSettings(
  settings: Settings,
): ResultAsync<SetSettingsOutcome, ShellError> {
  return fromTauri("set_settings", setSettingsOutcomeSchema, () =>
    invoke("set_settings", { settings }),
  );
}

// Re-pull cue: a settings write landed (from this window or the first-run flow), so live
// consumers reload from the store. The payload is unused — the handler always re-pulls.
export function subscribeSettingsChanged(
  handler: () => void,
): ResultAsync<UnlistenFn, ShellError> {
  return fromTauri(`listen:${SETTINGS_CHANGED_EVENT}`, unlistenSchema, () =>
    listen(SETTINGS_CHANGED_EVENT, () => {
      handler();
    }),
  );
}

// Enrol Beeline as a Login Item (SPEC §2, §13) via the autostart plugin. Offered once in the
// first-run flow; degraded operation without it never crashes (§13).
export function enableLoginItem(): ResultAsync<null, ShellError> {
  return fromTauri("plugin:autostart|enable", unitSchema, () =>
    invoke("plugin:autostart|enable"),
  );
}

// Open the Full Disk Access pane so the user can grant it (SPEC §13). Wrapped in the shell's
// Result pattern like every other crossing out of TypeScript.
export function openFullDiskAccessSettings(): ResultAsync<void, ShellError> {
  return ResultAsync.fromPromise(openUrl(FULL_DISK_ACCESS_URL), (cause) => ({
    code: "tauri-request-failed",
    operation: "opener.openUrl",
    cause,
  }));
}
