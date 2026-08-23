import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { err, ok, ResultAsync, type Result } from "neverthrow";
import { z } from "zod";

const WINDOW_SHOWN_EVENT = "beeline://window-shown";

// Tauri commands that return unit resolve to `null` across the IPC boundary.
const unitSchema = z.null();
const windowShownPayloadSchema = z
  .object({
    origin: z.enum(["launch", "shortcut"]),
    sequence: z.number().int().nonnegative(),
  })
  .strict();
const unlistenSchema = z.custom<UnlistenFn>(
  (value) => typeof value === "function",
);

export type ShellError =
  | { code: "invalid-boundary-payload"; operation: string; cause: z.ZodError }
  | { code: "tauri-request-failed"; operation: string; cause: unknown };

function parseBoundary<T>(
  operation: string,
  schema: z.ZodType<T>,
  payload: unknown,
): Result<T, ShellError> {
  const parsed = schema.safeParse(payload);
  return parsed.success
    ? ok(parsed.data)
    : err({ code: "invalid-boundary-payload", operation, cause: parsed.error });
}

function requestError(operation: string, cause: unknown): ShellError {
  return { code: "tauri-request-failed", operation, cause };
}

// One wrapper for every crossing out of TypeScript: a rejected promise becomes a
// Result error, and the resolved payload is parsed before it is trusted as `T`.
export function fromTauri<T>(
  operation: string,
  schema: z.ZodType<T>,
  call: () => Promise<unknown>,
): ResultAsync<T, ShellError> {
  return ResultAsync.fromPromise(call(), (cause) =>
    requestError(operation, cause),
  ).andThen((payload) => parseBoundary(operation, schema, payload));
}

function invokeCommand(
  command: string,
  args: Record<string, unknown>,
): ResultAsync<null, ShellError> {
  return fromTauri(command, unitSchema, () => invoke(command, args));
}

function recordTelemetry(
  name: string,
  fields: Record<string, unknown>,
): ResultAsync<null, ShellError> {
  return invokeCommand("telemetry_event", { name, fields });
}

function hideCurrentWindow(): ResultAsync<null, ShellError> {
  return fromTauri("window.hide", unitSchema, () => getCurrentWindow().hide());
}

function reportShellError(error: ShellError): void {
  console.error("Beeline shell operation failed", error);
}

function reportIfError(result: Result<unknown, ShellError>): void {
  if (result.isErr()) {
    reportShellError(result.error);
  }
}

function handleWindowShown(payload: unknown): void {
  parseBoundary(WINDOW_SHOWN_EVENT, windowShownPayloadSchema, payload).match(
    (shown) => {
      window.requestAnimationFrame(() => {
        void recordTelemetry("frontend_painted", shown).match(
          () => undefined,
          reportShellError,
        );
      });
    },
    reportShellError,
  );
}

function registerWindowShownListener(): ResultAsync<UnlistenFn, ShellError> {
  return fromTauri(`listen:${WINDOW_SHOWN_EVENT}`, unlistenSchema, () =>
    listen(WINDOW_SHOWN_EVENT, (event) => {
      handleWindowShown(event.payload);
    }),
  );
}

async function hideForEscape(): Promise<void> {
  reportIfError(await recordTelemetry("hide_requested", { origin: "escape" }));

  const hidden = await hideCurrentWindow();
  if (hidden.isErr()) {
    reportShellError(hidden.error);
    return;
  }

  reportIfError(await recordTelemetry("window_hidden", { origin: "escape" }));
}

function handleKeyDown(event: KeyboardEvent): void {
  if (event.defaultPrevented || event.key !== "Escape" || event.repeat) {
    return;
  }

  event.preventDefault();
  void hideForEscape();
}

export async function initializeShell(): Promise<void> {
  reportIfError(await registerWindowShownListener());

  document.addEventListener("keydown", handleKeyDown);

  reportIfError(await invokeCommand("frontend_ready", {}));
}
