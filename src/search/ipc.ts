import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { errAsync, okAsync, type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, reportShellError, type ShellError } from "../shell";

const QGRAM_READY_EVENT = "beeline://search-qgram-ready";

const tierSchema = z.enum(["normal", "hidden", "junk"]);
export type Tier = z.infer<typeof tierSchema>;

export const searchHitSchema = z
  .object({
    name: z.string(),
    path: z.string(),
    isDirectory: z.boolean(),
    tier: tierSchema,
  })
  .strict();
export type SearchHit = z.infer<typeof searchHitSchema>;

export const searchWaveSchema = z
  .object({
    stage: z.enum(["working_set", "exact", "fuzzy"]),
    revision: z.number().int().nonnegative(),
    complete: z.boolean(),
    qgramReady: z.boolean(),
    scanned: z.number().int().nonnegative(),
    backendDurationMs: z.number().int().nonnegative(),
    hits: z.array(searchHitSchema),
  })
  .strict();
export type SearchWave = z.infer<typeof searchWaveSchema>;

const unitSchema = z.null();
const readyPayloadSchema = z.object({}).strict();
const unlistenSchema = z.custom<UnlistenFn>(
  (value) => typeof value === "function",
);

// Start one Search v2 stream. Every channel message is parsed before the caller sees it;
// the command resolves only after its final wave or cancellation.
export function searchNameIndex(
  query: string,
  limit: number,
  currentLocation: string | null,
  pinnedPaths: string[],
  onWave: (wave: SearchWave) => void,
): ResultAsync<null, ShellError> {
  const boundaryErrors: ShellError[] = [];
  const channel = new Channel<unknown>((payload) => {
    const parsed = searchWaveSchema.safeParse(payload);
    if (parsed.success) {
      onWave(parsed.data);
    } else {
      boundaryErrors.push({
        code: "invalid-boundary-payload",
        operation: "search_name_index_v2:channel",
        cause: parsed.error,
      });
    }
  });
  return fromTauri("search_name_index_v2", unitSchema, () =>
    invoke("search_name_index_v2", {
      query,
      limit,
      currentLocation,
      pinnedPaths,
      onWave: channel,
    }),
  ).andThen((value) => {
    const error = boundaryErrors[0];
    return error === undefined ? okAsync(value) : errAsync(error);
  });
}

// The sidecar is built once in a helper process. A query entered while that work is still
// running first gets exact results; this event makes the active query transparently rerun
// as soon as global Typo Correction becomes available.
export function subscribeSearchQgramReady(
  handler: () => void,
): ResultAsync<UnlistenFn, ShellError> {
  return fromTauri(`listen:${QGRAM_READY_EVENT}`, unlistenSchema, () =>
    listen(QGRAM_READY_EVENT, (event) => {
      const parsed = readyPayloadSchema.safeParse(event.payload);
      if (parsed.success) {
        handler();
      } else {
        reportShellError({
          code: "invalid-boundary-payload",
          operation: QGRAM_READY_EVENT,
          cause: parsed.error,
        });
      }
    }),
  );
}

export type VisitKind = "entered_location" | "opened_file";
export type SearchSignalKind =
  | "action_menu"
  | "quick_look"
  | "completed_action";

export function recordVisit(
  path: string,
  kind: VisitKind,
): ResultAsync<null, ShellError> {
  return fromTauri("record_visit", unitSchema, () =>
    invoke("record_visit", { path, kind }),
  );
}

export function recordSearchSignal(
  path: string,
  query: string | null,
  kind: SearchSignalKind,
): ResultAsync<null, ShellError> {
  return fromTauri("record_search_signal", unitSchema, () =>
    invoke("record_search_signal", { path, query, kind }),
  );
}

export function resetLearnedRanking(): ResultAsync<null, ShellError> {
  return fromTauri("reset_learned_ranking", unitSchema, () =>
    invoke("reset_learned_ranking"),
  );
}
