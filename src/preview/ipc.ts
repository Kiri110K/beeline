import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";

// The three panel-less preview providers chunk A exposes (SPEC §9): metadata, a text
// excerpt, and a thumbnail PNG path. Each is Async class — fired fire-and-forget so focus
// movement never blocks, parsed at the boundary, and never allowed to overwrite a newer
// selection (the generation guard lives in the caller, SPEC §10).

// The tagged, code-only preview failure. Mirrors the Rust `PreviewError`
// (`#[serde(tag = "code", rename_all = "kebab-case")]`); parsed so the UI matches on the
// union, never on raw backend text.
export const previewErrorSchema = z.discriminatedUnion("code", [
  z.object({ code: z.literal("not-found") }),
  z.object({ code: z.literal("is-a-directory") }),
  z.object({ code: z.literal("binary") }),
  z.object({ code: z.literal("io") }),
]);
export type PreviewError = z.infer<typeof previewErrorSchema>;

// A rejected preview command carries the serde-tagged `PreviewError` as its cause; anything
// unrecognized collapses to a generic io failure so no raw backend text reaches the UI.
function classifyPreviewError(error: ShellError): PreviewError {
  if (error.code === "tauri-request-failed") {
    const parsed = previewErrorSchema.safeParse(error.cause);
    if (parsed.success) {
      return parsed.data;
    }
  }
  return { code: "io" };
}

// Lightweight header metadata (SPEC §9). `sizeBytes`/`childCount` are mutually exclusive:
// a file carries a byte size, a directory a non-recursive child count (no recursive size).
export const previewMetadataSchema = z
  .object({
    sizeBytes: z.number().int().nonnegative().nullable(),
    createdMs: z.number().int().nullable(),
    modifiedMs: z.number().int().nullable(),
    kind: z.string(),
    isDirectory: z.boolean(),
    childCount: z.number().int().nonnegative().nullable(),
  })
  .strict();
export type PreviewMetadata = z.infer<typeof previewMetadataSchema>;

// The head of a text/Markdown file, lossy UTF-8, with a flag for whether more followed.
export const textExcerptSchema = z
  .object({ text: z.string(), truncated: z.boolean() })
  .strict();
export type TextExcerpt = z.infer<typeof textExcerptSchema>;

// A generated thumbnail, delivered as a cache-dir PNG path (loaded via the asset protocol,
// never inline bytes — SPEC §9). `null` means a newer request for the same path superseded
// this one, so the caller ignores the stale reply.
export const thumbnailSchema = z
  .object({ pngPath: z.string() })
  .strict()
  .nullable();
export type Thumbnail = z.infer<typeof thumbnailSchema>;

export function previewMetadata(
  path: string,
): ResultAsync<PreviewMetadata, PreviewError> {
  return fromTauri("preview_metadata", previewMetadataSchema, () =>
    invoke("preview_metadata", { path }),
  ).mapErr(classifyPreviewError);
}

export function previewTextExcerpt(
  path: string,
  maxBytes: number,
): ResultAsync<TextExcerpt, PreviewError> {
  return fromTauri("preview_text_excerpt", textExcerptSchema, () =>
    invoke("preview_text_excerpt", { path, maxBytes }),
  ).mapErr(classifyPreviewError);
}

export function previewThumbnail(
  path: string,
  maxPx: number,
): ResultAsync<Thumbnail, PreviewError> {
  return fromTauri("preview_thumbnail", thumbnailSchema, () =>
    invoke("preview_thumbnail", { path, maxPx }),
  ).mapErr(classifyPreviewError);
}
