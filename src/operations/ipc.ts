import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";
import { strings } from "../strings";

const PROGRESS_EVENT = "beeline://operation-progress";
const FINISHED_EVENT = "beeline://operation-finished";

// A batch job id: the u64 the engine hands back, a non-negative integer over IPC.
const jobIdSchema = z.number().int().nonnegative();
export type JobId = z.infer<typeof jobIdSchema>;

const newPathSchema = z.string();
const unitSchema = z.null();
// resolve_installed_bundle returns the first installed bundle id, or null.
const bundleSchema = z.string().nullable();

// The engine's pre-dispatch failure (SPEC §13). Mirrors the serde-tagged `OpError`
// (kebab-case `code`); parsed at the boundary so the UI matches on the union, never on
// raw text. A rename collision is the load-bearing member (drives the inline message).
export const opErrorSchema = z.discriminatedUnion("code", [
  z.object({ code: z.literal("source-not-found"), path: z.string() }),
  z.object({ code: z.literal("target-not-found"), path: z.string() }),
  z.object({ code: z.literal("target-not-a-directory"), path: z.string() }),
  z.object({ code: z.literal("invalid-name"), name: z.string(), reason: z.string() }),
  z.object({ code: z.literal("name-collision"), path: z.string() }),
  z.object({ code: z.literal("command-failed"), detail: z.string() }),
  z.object({ code: z.literal("io"), detail: z.string() }),
]);
export type OpError = z.infer<typeof opErrorSchema>;

// A rejected operation command carries the serde-tagged `OpError` as its cause; anything
// unrecognized collapses to a generic io failure so no raw backend text reaches the UI.
function classifyOpError(error: ShellError): OpError {
  if (error.code === "tauri-request-failed") {
    const parsed = opErrorSchema.safeParse(error.cause);
    if (parsed.success) {
      return parsed.data;
    }
  }
  return { code: "io", detail: "operation failed" };
}

// A concrete, human sentence for a pre-dispatch failure, for the Status Strip or an
// inline rename message (SPEC §8, §13). `io`/`command-failed` already carry an
// engine-built English sentence; the rest map through the central strings.
export function opErrorMessage(error: OpError): string {
  switch (error.code) {
    case "source-not-found":
      return strings.operations.errors.sourceNotFound(error.path);
    case "target-not-found":
      return strings.operations.errors.targetNotFound(error.path);
    case "target-not-a-directory":
      return strings.operations.errors.targetNotADirectory(error.path);
    case "invalid-name":
      return strings.operations.errors.invalidName(error.reason);
    case "name-collision":
      return strings.operations.errors.nameCollision;
    case "command-failed":
      return error.detail;
    case "io":
      return error.detail;
  }
}

export function pasteCopy(
  sources: string[],
  targetDir: string,
): ResultAsync<JobId, OpError> {
  return fromTauri("paste_copy", jobIdSchema, () =>
    invoke("paste_copy", { sources, targetDir }),
  ).mapErr(classifyOpError);
}

export function pasteMove(
  sources: string[],
  targetDir: string,
): ResultAsync<JobId, OpError> {
  return fromTauri("paste_move", jobIdSchema, () =>
    invoke("paste_move", { sources, targetDir }),
  ).mapErr(classifyOpError);
}

export function trashItems(paths: string[]): ResultAsync<JobId, OpError> {
  return fromTauri("trash_items", jobIdSchema, () =>
    invoke("trash_items", { paths }),
  ).mapErr(classifyOpError);
}

export function deleteItemsPermanently(
  paths: string[],
): ResultAsync<JobId, OpError> {
  return fromTauri("delete_items_permanently", jobIdSchema, () =>
    invoke("delete_items_permanently", { paths }),
  ).mapErr(classifyOpError);
}

// Rename in place; resolves to the new absolute path, rejects with `NameCollision`
// (shown inline) or another `OpError` (SPEC §8).
export function renameItem(
  path: string,
  newName: string,
): ResultAsync<string, OpError> {
  return fromTauri("rename_item", newPathSchema, () =>
    invoke("rename_item", { path, newName }),
  ).mapErr(classifyOpError);
}

// Create a folder and resolve its created path; New Folder then enters inline rename.
export function createFolder(
  parentDir: string,
  name: string,
): ResultAsync<string, OpError> {
  return fromTauri("create_folder", newPathSchema, () =>
    invoke("create_folder", { parentDir, name }),
  ).mapErr(classifyOpError);
}

export function revealInFinder(path: string): ResultAsync<null, OpError> {
  return fromTauri("reveal_in_finder", unitSchema, () =>
    invoke("reveal_in_finder", { path }),
  ).mapErr(classifyOpError);
}

export function openInApp(
  path: string,
  bundleId: string,
): ResultAsync<null, OpError> {
  return fromTauri("open_in_app", unitSchema, () =>
    invoke("open_in_app", { path, bundleId }),
  ).mapErr(classifyOpError);
}

// Best-effort cancellation of a running batch (SPEC §10). Unknown ids are a no-op.
export function cancelOperation(jobId: JobId): ResultAsync<null, ShellError> {
  return fromTauri("cancel_operation", unitSchema, () =>
    invoke("cancel_operation", { jobId }),
  );
}

// Resolve the first installed bundle id from a priority list, for Terminal/Editor slot
// auto-seeding (SPEC §8). `null` means none is installed.
export function resolveInstalledBundle(
  bundleIds: string[],
): ResultAsync<string | null, ShellError> {
  return fromTauri("resolve_installed_bundle", bundleSchema, () =>
    invoke("resolve_installed_bundle", { bundleIds }),
  );
}

// One `beeline://operation-progress` tick. Field names are the engine's snake_case wire
// contract, parsed here and mapped to camelCase for the rest of the frontend.
const progressPayloadSchema = z
  .object({
    job_id: jobIdSchema,
    done: z.number().int().nonnegative(),
    total: z.number().int().nonnegative(),
    current_path: z.string(),
  })
  .strict();
export interface OperationProgress {
  jobId: JobId;
  done: number;
  total: number;
  currentPath: string;
}

const failureSchema = z
  .object({ path: z.string(), cause: z.string() })
  .strict();
export type OperationFailure = z.infer<typeof failureSchema>;

const operationKindSchema = z.enum([
  "paste_copy",
  "paste_move",
  "trash",
  "delete_permanently",
]);
const pathChangeSchema = z
  .object({ previous_path: z.string(), next_path: z.string() })
  .strict();

const finishedPayloadSchema = z
  .object({
    job_id: jobIdSchema,
    kind: operationKindSchema,
    ok_count: z.number().int().nonnegative(),
    failures: z.array(failureSchema),
    completed_paths: z.array(z.string()),
    path_changes: z.array(pathChangeSchema),
  })
  .strict();
export interface OperationFinished {
  jobId: JobId;
  kind: z.infer<typeof operationKindSchema>;
  okCount: number;
  failures: OperationFailure[];
  completedPaths: string[];
  pathChanges: { previousPath: string; nextPath: string }[];
}

const unlistenSchema = z.custom<UnlistenFn>(
  (value) => typeof value === "function",
);

export function subscribeOperationProgress(
  handler: (progress: OperationProgress) => void,
): ResultAsync<UnlistenFn, ShellError> {
  return fromTauri(`listen:${PROGRESS_EVENT}`, unlistenSchema, () =>
    listen(PROGRESS_EVENT, (event) => {
      const parsed = progressPayloadSchema.safeParse(event.payload);
      if (parsed.success) {
        handler({
          jobId: parsed.data.job_id,
          done: parsed.data.done,
          total: parsed.data.total,
          currentPath: parsed.data.current_path,
        });
      }
    }),
  );
}

export function subscribeOperationFinished(
  handler: (finished: OperationFinished) => void,
): ResultAsync<UnlistenFn, ShellError> {
  return fromTauri(`listen:${FINISHED_EVENT}`, unlistenSchema, () =>
    listen(FINISHED_EVENT, (event) => {
      const parsed = finishedPayloadSchema.safeParse(event.payload);
      if (parsed.success) {
        handler({
          jobId: parsed.data.job_id,
          kind: parsed.data.kind,
          okCount: parsed.data.ok_count,
          failures: parsed.data.failures,
          completedPaths: parsed.data.completed_paths,
          pathChanges: parsed.data.path_changes.map((change) => ({
            previousPath: change.previous_path,
            nextPath: change.next_path,
          })),
        });
      }
    }),
  );
}
