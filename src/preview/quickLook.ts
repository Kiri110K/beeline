import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";

// The native Quick Look bridge (chunk A, SPEC §5/§9). The frontend owns navigation while the
// panel is up: Space opens it on the current file list, Up/Down re-point it via
// `quick_look_update`, and Escape / Space-again close it via `quick_look_hide`. The
// `beeline://quick-look-closed` event keeps the frontend's "open" mirror honest when the
// panel is closed by its own close control instead of by the app (SPEC §5 Escape order).

const QUICK_LOOK_CLOSED_EVENT = "beeline://quick-look-closed";

// Every Quick Look command resolves to unit across the boundary.
const unitSchema = z.null();
const unlistenSchema = z.custom<UnlistenFn>(
  (value) => typeof value === "function",
);

// Open the panel on `paths`, focused at `index`. Async-dispatched in Rust, so this returns
// within the ≤50 ms Quick Look budget (SPEC §10); the caller adds no awaits before it.
export function quickLookShow(
  paths: string[],
  index: number,
): ResultAsync<null, ShellError> {
  return fromTauri("quick_look_show", unitSchema, () =>
    invoke("quick_look_show", { paths, index }),
  );
}

// Replace the open panel's list/position (Up/Down while open). A no-op in Rust when closed.
export function quickLookUpdate(
  paths: string[],
  index: number,
): ResultAsync<null, ShellError> {
  return fromTauri("quick_look_update", unitSchema, () =>
    invoke("quick_look_update", { paths, index }),
  );
}

// Close the panel programmatically (Space-again / Escape order). Rust flips the mirror and
// stays silent on the closed event, so this never round-trips back as a spurious close.
export function quickLookHide(): ResultAsync<null, ShellError> {
  return fromTauri("quick_look_hide", unitSchema, () =>
    invoke("quick_look_hide", {}),
  );
}

// Fire when the panel is closed by the user's own close control (not by the app). The
// payload is unit; the handler only needs the signal to drop the "open" mirror.
export function subscribeQuickLookClosed(
  handler: () => void,
): ResultAsync<UnlistenFn, ShellError> {
  return fromTauri(`listen:${QUICK_LOOK_CLOSED_EVENT}`, unlistenSchema, () =>
    listen(QUICK_LOOK_CLOSED_EVENT, () => {
      handler();
    }),
  );
}
