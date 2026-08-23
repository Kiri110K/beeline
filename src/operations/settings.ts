import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";

// The two auto-seeded application slots (SPEC §8, §12). Persisted Rust-side; the full
// Settings UI is #30, so this is storage plus the resolution seam only. Parsed at the
// boundary; `null` means "not yet seeded".
const appSettingsSchema = z
  .object({
    terminalBundleId: z.string().nullable(),
    editorBundleId: z.string().nullable(),
  })
  .strict();
export type AppSettings = z.infer<typeof appSettingsSchema>;

// Known-app priority lists (SPEC §8): the first installed one wins on first use.
export const TERMINAL_BUNDLE_IDS = [
  "com.mitchellh.ghostty",
  "com.googlecode.iterm2",
  "com.apple.Terminal",
] as const;
export const EDITOR_BUNDLE_IDS = [
  "dev.zed.Zed",
  "com.microsoft.VSCode",
] as const;

export type Slot = "terminal" | "editor";

export function loadAppSettings(): ResultAsync<AppSettings, ShellError> {
  return fromTauri("load_app_settings", appSettingsSchema, () =>
    invoke("load_app_settings"),
  );
}

const unitSchema = z.null();

export function saveAppSettings(
  settings: AppSettings,
): ResultAsync<null, ShellError> {
  return fromTauri("save_app_settings", unitSchema, () =>
    invoke("save_app_settings", { settings }),
  );
}
