import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";
import { itemSchema } from "./schema";

// Why Spotlight could not serve Recents (SPEC §7). Mirrors the Rust `UnavailableReason`.
export const recentsUnavailableReasonSchema = z.enum([
  "disabled",
  "privacy_excluded",
  "indexing",
]);
export type RecentsUnavailableReason = z.infer<
  typeof recentsUnavailableReasonSchema
>;

// The `get_recents` response, tagged by `state` so honest-empty and Spotlight-
// unavailable stay distinct (SPEC §7). `ok` carries a page plus the full cached count
// for progressive paging; the Items share the directory-listing schema.
export const recentsResponseSchema = z.discriminatedUnion("state", [
  z
    .object({
      state: z.literal("ok"),
      items: z.array(itemSchema),
      total: z.number().int().nonnegative(),
    })
    .strict(),
  z.object({ state: z.literal("empty") }).strict(),
  z
    .object({
      state: z.literal("spotlight_unavailable"),
      reason: recentsUnavailableReasonSchema,
    })
    .strict(),
]);
export type RecentsResponse = z.infer<typeof recentsResponseSchema>;

// Fetch a page of the Recents collection (SPEC §7). Serves from the last-success cache
// instantly and triggers a background refresh; the refresh notifies via the
// `beeline://recents-updated` event so the caller re-pulls.
export function getRecents(
  offset: number,
  limit: number,
): ResultAsync<RecentsResponse, ShellError> {
  return fromTauri("get_recents", recentsResponseSchema, () =>
    invoke("get_recents", { offset, limit }),
  );
}
