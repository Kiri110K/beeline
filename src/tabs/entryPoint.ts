// The Default Entry Point (CONTEXT.md) of a newly created Temporary Tab.
//
// Version 1 resolves it to the home directory. A later ticket makes Recents the
// default and routes the choice through Settings; that change lands entirely
// inside this function, which is why callers pass the already-resolved home and
// take a plain Location back rather than reaching for the home directory
// themselves. This is the seam, not a placeholder to delete.
export function defaultEntryPoint(home: string): string {
  return home;
}
