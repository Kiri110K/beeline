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
