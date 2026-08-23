import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import type { Item } from "../location/schema";
import { fromTauri, type ShellError } from "../shell";

// The primary action of an Item, resolved by kind. One small chokepoint so
// Settings (later chunk) can replace the hardcoded per-kind defaults (§5).
export type PrimaryAction =
  | { kind: "enter"; path: string }
  | { kind: "open"; path: string };

// v1 defaults: a directory is entered, a file opens in the system default app.
export function primaryActionFor(item: Item): PrimaryAction {
  return item.isDirectory
    ? { kind: "enter", path: item.path }
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
