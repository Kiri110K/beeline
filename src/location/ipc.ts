import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";
import {
  initialListLocationSchema,
  listErrorSchema,
  listLocationSchema,
  type InitialListLocationResponse,
  type ListErrorPayload,
  type ListLocationResponse,
} from "./schema";

const homeDirectorySchema = z.string();

// A rejected `list_location` carries the serde-tagged domain error as its cause;
// anything the schema does not recognize collapses to a generic io failure so no
// raw backend text reaches the UI.
function classifyListError(error: ShellError): ListErrorPayload {
  if (error.code === "tauri-request-failed") {
    const parsed = listErrorSchema.safeParse(error.cause);
    if (parsed.success) {
      return parsed.data;
    }
  }
  return { code: "io" };
}

export function listLocationInitial(
  path: string,
): ResultAsync<InitialListLocationResponse, ListErrorPayload> {
  return fromTauri("list_location_initial", initialListLocationSchema, () =>
    invoke("list_location_initial", { path }),
  ).mapErr(classifyListError);
}

export function listLocation(
  path: string,
): ResultAsync<ListLocationResponse, ListErrorPayload> {
  return fromTauri("list_location", listLocationSchema, () =>
    invoke("list_location", { path }),
  ).mapErr(classifyListError);
}

// Validate a Default Entry Point candidate (SPEC §4, §12): a cheap metadata-only check that
// resolves to the accepted path or the same tagged domain error as a listing, so Settings can
// gate the Folder choice without ever persisting an empty, missing, or file path.
export function validateDirectory(
  path: string,
): ResultAsync<string, ListErrorPayload> {
  return fromTauri("validate_directory", z.string(), () =>
    invoke("validate_directory", { path }),
  ).mapErr(classifyListError);
}

export function homeDirectory(): ResultAsync<string, ShellError> {
  return fromTauri("home_directory", homeDirectorySchema, () =>
    invoke("home_directory"),
  );
}
