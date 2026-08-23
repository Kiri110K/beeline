// What a Tab is browsing (CONTEXT.md: Location, Recents). A Tab shows either a
// directory or the system-derived Recents collection, so the two are a discriminated
// union rather than a magic path string — the compiler then forces every consumer to
// say which it means.
export type Location =
  | { kind: "directory"; path: string }
  | { kind: "recents" };

export function directoryLocation(path: string): Location {
  return { kind: "directory", path };
}

export const recentsLocation: Location = { kind: "recents" };

export function locationEquals(a: Location, b: Location): boolean {
  if (a.kind === "directory" && b.kind === "directory") {
    return a.path === b.path;
  }
  return a.kind === b.kind;
}
