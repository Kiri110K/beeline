import { recentsLocation, type Location } from "../location/location";

// The Default Entry Point (CONTEXT.md) of a newly created Temporary Tab.
//
// Version 1 resolves it to the Recents collection (SPEC §4/§7). A later ticket routes
// the choice through Settings, letting the user pick a directory instead; that change
// lands entirely inside this function, which is why callers take an already-resolved
// Location back rather than deciding for themselves. This is the seam, not a placeholder.
export function defaultEntryPoint(): Location {
  return recentsLocation;
}
