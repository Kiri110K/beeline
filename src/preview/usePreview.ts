import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";

import { recordTelemetry, reportShellError } from "../shell";
import {
  previewMetadata,
  previewTextExcerpt,
  previewThumbnail,
} from "./ipc";
import {
  classifyPreview,
  PREVIEW_TEXT_MAX_BYTES,
  PREVIEW_THUMBNAIL_MAX_PX,
  type PreviewKind,
  type PreviewTarget,
} from "./model";
import type { PreviewMetadata } from "./ipc";

// The header metadata slice: pending until `preview_metadata` lands, then the data, or a
// "missing" state when the Item vanished before the read (SPEC §9).
export type PreviewMetaView =
  | { status: "pending" }
  | { status: "ready"; metadata: PreviewMetadata }
  | { status: "missing" };

// The body slice: the lightweight preview under the header. `none` covers a directory, a
// generic-icon file, and any failed excerpt/thumbnail — all render a kind glyph.
export type PreviewBodyView =
  | { status: "pending" }
  | { status: "none" }
  | { status: "image"; src: string }
  | { status: "text"; text: string; truncated: boolean };

export interface PreviewView {
  target: PreviewTarget;
  meta: PreviewMetaView;
  body: PreviewBodyView;
}

// Async results, tagged by the path they belong to: a stale reply for a path the focus has
// since left is ignored during render (the generation guard drops it before it ever lands).
interface PreviewAsyncState {
  path: string;
  meta: PreviewMetaView;
  body: PreviewBodyView;
}

// Sampled `preview_shown {kind}` telemetry: one line every Nth preview so the focus-movement
// path stays cheap (SPEC §9 telemetry). Module-scoped so the count spans remounts.
const PREVIEW_TELEMETRY_SAMPLE = 20;
// Do not enqueue filesystem work for every row crossed while a key is held or a wheel is
// moving. The target and pending state still paint immediately; only the async preview waits
// for a short focus dwell. Without this coalescing, thousands of stale IPC promises can fill
// WebContent memory and the shared blocking pool before their generation guards drop replies.
const PREVIEW_REQUEST_DELAY_MS = 150;
let previewShownCount = 0;

function recordPreviewShown(kind: string): void {
  previewShownCount += 1;
  if (previewShownCount % PREVIEW_TELEMETRY_SAMPLE === 0) {
    void recordTelemetry("preview_shown", { kind }).match(
      () => undefined,
      reportShellError,
    );
  }
}

// The body a file/directory starts in before any async request resolves.
function initialBodyFor(isDirectory: boolean, kind: PreviewKind): PreviewBodyView {
  return isDirectory || kind === "generic"
    ? { status: "none" }
    : { status: "pending" };
}

// Follow the Focused Item (SPEC §9): the header and pending body are derived during render
// from the target, so focus movement paints immediately; the async metadata and body only
// arrive through state updates in promise callbacks. A per-target generation guards every
// commit so a stale reply never overwrites a newer selection (SPEC §10); requests are
// fire-and-forget, so focus movement is never blocked.
export function usePreview(target: PreviewTarget | null): PreviewView | null {
  const [asyncState, setAsync] = useState<PreviewAsyncState | null>(null);
  const generationRef = useRef(0);

  const path = target?.path ?? null;
  const name = target?.name ?? null;
  const isDirectory = target?.isDirectory ?? null;

  useEffect(() => {
    if (path === null || name === null || isDirectory === null) {
      // No target: bump the generation so any in-flight reply is dropped. The empty view is
      // derived during render, so no state update is needed here.
      generationRef.current += 1;
      return;
    }

    const generation = generationRef.current + 1;
    generationRef.current = generation;
    const kind: PreviewKind = isDirectory ? "generic" : classifyPreview(name);
    const startBody = initialBodyFor(isDirectory, kind);
    recordPreviewShown(isDirectory ? "directory" : kind);

    // Merge one async result into this target's slice, initializing the slice on the first
    // reply and ignoring anything from a superseded generation (SPEC §10).
    const commit = (
      apply: (base: PreviewAsyncState) => PreviewAsyncState,
    ): void => {
      if (generationRef.current !== generation) {
        return;
      }
      setAsync((prev) =>
        apply(
          prev !== null && prev.path === path
            ? prev
            : { path, meta: { status: "pending" }, body: startBody },
        ),
      );
    };

    const timer = window.setTimeout(() => {
      void previewMetadata(path).match(
        (metadata) => {
          commit((base) => ({ ...base, meta: { status: "ready", metadata } }));
        },
        () => {
          commit((base) => ({ ...base, meta: { status: "missing" } }));
        },
      );

      switch (kind) {
        case "text":
          void previewTextExcerpt(path, PREVIEW_TEXT_MAX_BYTES).match(
            (excerpt) => {
              commit((base) => ({
                ...base,
                body: {
                  status: "text",
                  text: excerpt.text,
                  truncated: excerpt.truncated,
                },
              }));
            },
            // A binary/vanished/unreadable excerpt falls back to a generic icon.
            () => {
              commit((base) => ({ ...base, body: { status: "none" } }));
            },
          );
          break;
        case "thumbnail":
          void previewThumbnail(path, PREVIEW_THUMBNAIL_MAX_PX).match(
            (thumbnail) => {
              // `null` means a newer request superseded this one — leave the pending body for
              // the newer request to resolve (SPEC §10: stale results are dropped).
              if (thumbnail === null) {
                return;
              }
              commit((base) => ({
                ...base,
                body: { status: "image", src: convertFileSrc(thumbnail.pngPath) },
              }));
            },
            // No thumbnail (unsupported/failed): fall back to a generic icon.
            () => {
              commit((base) => ({ ...base, body: { status: "none" } }));
            },
          );
          break;
        case "generic":
          break;
      }
    }, PREVIEW_REQUEST_DELAY_MS);

    return () => {
      window.clearTimeout(timer);
    };
  }, [path, name, isDirectory]);

  if (target === null) {
    return null;
  }
  const kind: PreviewKind = target.isDirectory
    ? "generic"
    : classifyPreview(target.name);
  const matches = asyncState !== null && asyncState.path === target.path;
  return {
    target,
    meta: matches ? asyncState.meta : { status: "pending" },
    body: matches ? asyncState.body : initialBodyFor(target.isDirectory, kind),
  };
}
