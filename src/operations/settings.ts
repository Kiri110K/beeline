// The two auto-seeded application slots (SPEC §8, §12): the known-app priority lists and the
// slot identifier. Storage and the full Settings surface now live in the settings store
// (`src/settings/`); this file carries only the resolution seed lists and the `Slot` type.

// Known-app priority lists (SPEC §8): the first installed one wins on first use.
export const TERMINAL_BUNDLE_IDS = [
  "com.mitchellh.ghostty",
  "com.googlecode.iterm2",
  "com.apple.Terminal",
] as const;
export const EDITOR_BUNDLE_IDS = [
  "dev.zed.Zed",
  "com.microsoft.VSCode",
] as const;

export type Slot = "terminal" | "editor";

// The two auto-seeded slots as one value, so the seed decision below is a pure function of the
// current settings and the freshly detected candidates.
export interface SlotBundleIds {
  terminalBundleId: string | null;
  editorBundleId: string | null;
}

// The combined slot update for first-run auto-seeding (SPEC §8, §12), or `null` when nothing
// changes. Rules: fill an empty slot with its detected candidate, never overwrite a slot the
// user (or another write in flight) already filled, and leave an empty slot empty when no
// candidate is installed. `current` is read at save time, so a slot filled during detection is
// preserved; `resolved` carries what the priority lists detected for the then-empty slots.
export function seededSlotUpdate(
  current: SlotBundleIds,
  resolved: SlotBundleIds,
): SlotBundleIds | null {
  const terminalBundleId = current.terminalBundleId ?? resolved.terminalBundleId;
  const editorBundleId = current.editorBundleId ?? resolved.editorBundleId;
  if (
    terminalBundleId === current.terminalBundleId &&
    editorBundleId === current.editorBundleId
  ) {
    return null;
  }
  return { terminalBundleId, editorBundleId };
}
