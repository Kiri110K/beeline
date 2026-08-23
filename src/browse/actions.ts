import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import type { Item } from "../location/schema";
import type { Settings } from "../settings/schema";
import { fromTauri, type ShellError } from "../shell";

// The primary action of an Item, resolved by kind from Settings (SPEC §5, §12). A directory
// is entered or shows the Action Menu; a file opens or shows the Action Menu — the one
// chokepoint the whole app consults.
export type PrimaryAction =
  | { kind: "enter"; path: string }
  | { kind: "open"; path: string }
  | { kind: "menu" };

// Resolve an Item's primary action against the configured per-kind choice (SPEC §5 defaults:
// directory → Enter Location, file → Open with Default App; either may be Show Action Menu).
export function primaryActionFor(item: Item, settings: Settings): PrimaryAction {
  if (item.isDirectory) {
    return settings.primaryActionDirectory === "menu"
      ? { kind: "menu" }
      : { kind: "enter", path: item.path };
  }
  return settings.primaryActionFile === "menu"
    ? { kind: "menu" }
    : { kind: "open", path: item.path };
}

// The opener plugin's open_path command resolves to unit (`null`) across IPC.
const openResultSchema = z.null();

// Open a path with the system default application via tauri-plugin-opener,
// wrapped like every other crossing out of TypeScript. Requires the
// `opener:allow-open-path` capability.
export function openPath(path: string): ResultAsync<null, ShellError> {
  return fromTauri("plugin:opener|open_path", openResultSchema, () =>
    invoke("plugin:opener|open_path", { path }),
  );
}
