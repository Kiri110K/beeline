import {
  directoryLocation,
  recentsLocation,
  type Location,
} from "../location/location";
import type { EntryPointSetting } from "../settings/schema";

// The Default Entry Point (CONTEXT.md) of a newly created Temporary Tab, resolved from
// Settings (SPEC §4, §12): Recents unless the user picked a directory. Callers take an
// already-resolved Location so the choice lives entirely here — the seam.
export function entryPointLocation(setting: EntryPointSetting): Location {
  return setting.kind === "recents"
    ? recentsLocation
    : directoryLocation(setting.path);
}
