import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";

// The persisted shape of one Pinned Tab: Anchor, custom name, and (implicitly)
// order. Nothing else about a Tab survives restart (§11). Parsed at the boundary.
const persistedPinnedTabSchema = z
  .object({
    anchorPath: z.string(),
    customName: z.string().nullable(),
  })
  .strict();
export type PersistedPinnedTab = z.infer<typeof persistedPinnedTabSchema>;

const persistedPinnedTabsSchema = z.array(persistedPinnedTabSchema);

export function loadPinnedTabs(): ResultAsync<PersistedPinnedTab[], ShellError> {
  return fromTauri("load_pinned_tabs", persistedPinnedTabsSchema, () =>
    invoke("load_pinned_tabs"),
  );
}

const unitSchema = z.null();

export function savePinnedTabs(
  tabs: PersistedPinnedTab[],
): ResultAsync<null, ShellError> {
  return fromTauri("save_pinned_tabs", unitSchema, () =>
    invoke("save_pinned_tabs", { tabs }),
  );
}
