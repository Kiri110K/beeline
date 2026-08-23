import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { err, ok, ResultAsync, type Result } from "neverthrow";
import { z } from "zod";

const WINDOW_SHOWN_EVENT = "beeline://window-shown";

// Tauri commands that return unit resolve to `null` across the IPC boundary.
const unitSchema = z.null();
const windowShownPayloadSchema = z
  .object({
    origin: z.enum(["launch", "shortcut"]),
    sequence: z.number().int().nonnegative(),
    // Continuous background ms before this show; null on the first launch show.
    hiddenMs: z.number().int().nonnegative().nullable(),
  })
  .strict();
export type WindowShownPayload = z.infer<typeof windowShownPayloadSchema>;
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

export function recordTelemetry(
  name: string,
  fields: Record<string, unknown>,
): ResultAsync<null, ShellError> {
  return invokeCommand("telemetry_event", { name, fields });
}

// Hide through Rust so the background clock is stamped in one place; the origin
// distinguishes an Escape hide from a Pinned-Tab Cmd+W hide in telemetry.
export function requestHide(origin: string): ResultAsync<null, ShellError> {
  return invokeCommand("hide_window", { origin });
}

// Copy text to the clipboard via the webview API (works in WKWebView), wrapped
// in the shell's Result pattern like every other side-effecting boundary.
export function copyToClipboard(text: string): ResultAsync<null, ShellError> {
  return ResultAsync.fromPromise(
    navigator.clipboard.writeText(text),
    (cause) => requestError("clipboard.writeText", cause),
  ).map(() => null);
}

export function reportShellError(error: ShellError): void {
  console.error("Beeline shell operation failed", error);
}

function reportIfError(result: Result<unknown, ShellError>): void {
  if (result.isErr()) {
    reportShellError(result.error);
  }
}

// Parsed subscription to the window-shown event, shared by the shell's own
// paint telemetry and the Tabs lifecycle listener (§9).
export function subscribeWindowShown(
  handler: (payload: WindowShownPayload) => void,
): ResultAsync<UnlistenFn, ShellError> {
  return fromTauri(`listen:${WINDOW_SHOWN_EVENT}`, unlistenSchema, () =>
    listen(WINDOW_SHOWN_EVENT, (event) => {
      parseBoundary(
        WINDOW_SHOWN_EVENT,
        windowShownPayloadSchema,
        event.payload,
      ).match(handler, reportShellError);
    }),
  );
}

function registerWindowShownListener(): ResultAsync<UnlistenFn, ShellError> {
  return subscribeWindowShown((shown) => {
    window.requestAnimationFrame(() => {
      void recordTelemetry("frontend_painted", shown).match(
        () => undefined,
        reportShellError,
      );
    });
  });
}

function handleKeyDown(event: KeyboardEvent): void {
  if (event.defaultPrevented || event.key !== "Escape" || event.repeat) {
    return;
  }

  event.preventDefault();
  void requestHide("escape").match(() => undefined, reportShellError);
}

export async function initializeShell(): Promise<void> {
  reportIfError(await registerWindowShownListener());

  document.addEventListener("keydown", handleKeyDown);

  reportIfError(await invokeCommand("frontend_ready", {}));
}
