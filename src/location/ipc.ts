import { invoke } from "@tauri-apps/api/core";
import { type ResultAsync } from "neverthrow";
import { z } from "zod";

import { fromTauri, type ShellError } from "../shell";
import {
  listErrorSchema,
  listLocationSchema,
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

export function listLocation(
  path: string,
): ResultAsync<ListLocationResponse, ListErrorPayload> {
  return fromTauri("list_location", listLocationSchema, () =>
    invoke("list_location", { path }),
  ).mapErr(classifyListError);
}

export function homeDirectory(): ResultAsync<string, ShellError> {
  return fromTauri("home_directory", homeDirectorySchema, () =>
    invoke("home_directory"),
  );
}
