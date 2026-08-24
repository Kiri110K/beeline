import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";

// One ranked Search Result crossing the IPC boundary. Mirrors the Rust `SearchHit`
// (camelCase serde rename); parsed here, trusted afterwards.
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

// The Rust `SearchResponse`: the ranked hits plus the index generation they were
// computed against (retained for future progressive reconciliation, SPEC §6), and scan
// metadata (`reused`, `scanned`) passed through to sampled search telemetry (SPEC §10).
export const searchResponseSchema = z
  .object({
    revision: z.number().int().nonnegative(),
    reused: z.boolean(),
    scanned: z.number().int().nonnegative(),
    backendDurationMs: z.number().int().nonnegative(),
    hits: z.array(searchHitSchema),
  })
  .strict();
export type SearchResponse = z.infer<typeof searchResponseSchema>;

// Query the Name Index for up to `limit` ranked hits (SPEC §6). The backend trims
// and lowercases the query itself; the raw text is passed straight through.
export function searchNameIndex(
  query: string,
  limit: number,
): ResultAsync<SearchResponse, ShellError> {
  return fromTauri("search_name_index", searchResponseSchema, () =>
    invoke("search_name_index", { query, limit }),
  );
}

// What a visit was (SPEC §6): a Location entered, or a file opened. Only these two
// wire strings are accepted by the `record_visit` command.
export type VisitKind = "entered_location" | "opened_file";

const unitSchema = z.null();

// Record a visit into the Visit Journal — a ranking-only signal, never user-facing
// (SPEC §6). Recording is liberal in v1; failures are reported, never surfaced.
export function recordVisit(
  path: string,
  kind: VisitKind,
): ResultAsync<null, ShellError> {
  return fromTauri("record_visit", unitSchema, () =>
    invoke("record_visit", { path, kind }),
  );
}
