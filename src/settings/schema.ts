import { z } from "zod";

import {
  AFTER_ACTION_IDS,
  DEFAULT_AFTER_ACTION,
  type AfterActionTable,
  type AfterEffect,
} from "../operations/afterAction";

// The persisted settings surface (SPEC §12), parsed at the IPC boundary before it is
// trusted (docs/agents/typescript.md: parse, don't validate). The shapes mirror the Rust
// `AppSettings` serde output field-for-field. `firstRunDismissed` is internal persistence
// (SPEC §13), not a user setting — it never appears in the Settings view.

// The global shortcut, modelled by the browser's own `KeyboardEvent.code` plus modifier
// flags so the capture field and the Rust `Shortcut` share one representation (SPEC §2).
export const shortcutSpecSchema = z
  .object({
    control: z.boolean(),
    alt: z.boolean(),
    shift: z.boolean(),
    meta: z.boolean(),
    code: z.string(),
  })
  .strict();
export type ShortcutSpec = z.infer<typeof shortcutSpecSchema>;

// The Default Entry Point of a new Temporary Tab (SPEC §4): Recents or a typed directory.
export const entryPointSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("recents") }).strict(),
  z.object({ kind: z.literal("directory"), path: z.string() }).strict(),
]);
export type EntryPointSetting = z.infer<typeof entryPointSchema>;

// The Temporary Tab lifetime (SPEC §4): a positive minute count, or never expiring.
export const lifetimeSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("minutes"), minutes: z.number().int().positive() }).strict(),
  z.object({ kind: z.literal("never") }).strict(),
]);
export type LifetimeSetting = z.infer<typeof lifetimeSchema>;

export const primaryActionDirectorySchema = z.enum(["enter", "menu"]);
export type PrimaryActionDirectory = z.infer<typeof primaryActionDirectorySchema>;
export const primaryActionFileSchema = z.enum(["open", "menu"]);
export type PrimaryActionFile = z.infer<typeof primaryActionFileSchema>;

const afterEffectSchema = z.enum(["hide", "keep"]);

export const aliasEntrySchema = z
  .object({ word: z.string(), path: z.string() })
  .strict();
export type AliasEntry = z.infer<typeof aliasEntrySchema>;

// The After Action table arrives as an open string→effect map; fill any missing action with
// its default so consumers index a total table (SPEC §12).
const afterActionRawSchema = z.record(z.string(), afterEffectSchema);
function normalizeAfterAction(
  raw: Record<string, AfterEffect>,
): AfterActionTable {
  const table: AfterActionTable = { ...DEFAULT_AFTER_ACTION };
  for (const id of AFTER_ACTION_IDS) {
    const value = raw[id];
    if (value !== undefined) {
      table[id] = value;
    }
  }
  return table;
}

export const settingsSchema = z
  .object({
    globalShortcut: shortcutSpecSchema,
    defaultEntryPoint: entryPointSchema,
    temporaryTabLifetime: lifetimeSchema,
    primaryActionDirectory: primaryActionDirectorySchema,
    primaryActionFile: primaryActionFileSchema,
    afterAction: afterActionRawSchema.transform(normalizeAfterAction),
    previewPanelVisible: z.boolean(),
    terminalBundleId: z.string().nullable(),
    editorBundleId: z.string().nullable(),
    aliases: z.array(aliasEntrySchema),
    junkPatterns: z.array(z.string()),
    // The seed version behind `junkPatterns` (SPEC §12). Internal persistence, like
    // `firstRunDismissed`: never rendered in the Settings UI, but parsed and carried so it
    // round-trips back through `set_settings` unchanged. If the frontend dropped it, the
    // backend would re-read a stale/zero version and re-run the Junk-seed migration, undoing
    // a user's deletion of a built-in Junk pattern.
    junkSeedVersion: z.number().int().nonnegative(),
    firstRunDismissed: z.boolean(),
  })
  .strict();
export type Settings = z.infer<typeof settingsSchema>;

// The built-in Junk seed list (SPEC §6), mirroring `name_index::junk::default_names` in Rust
// (the two languages cannot share the constant). Used for the fallback defaults and the
// Junk-editor's reset-to-default control.
export const DEFAULT_JUNK_PATTERNS: readonly string[] = [
  "node_modules",
  ".git",
  "target",
  ".build",
  "dist",
  ".cache",
  "Caches",
  ".claude",
  ".codex",
  // Junk seed v2 (mirrors `junk::V2_SEED_NAMES`): macOS app-data directories.
  "Containers",
  "Group Containers",
  "Application Support",
  "Logs",
  "Saved Application State",
];

// The current Junk seed version, mirroring `CURRENT_JUNK_SEED_VERSION` in `settings.rs` (the
// two languages cannot share the constant). Bumped in lockstep with `DEFAULT_JUNK_PATTERNS`
// whenever new built-in Junk names ship, so the fallback defaults below carry the right stamp.
export const CURRENT_JUNK_SEED_VERSION = 2;

// The pre-load fallback (SPEC §12 defaults). The real values arrive from `get_settings`
// immediately on startup; this only bridges the first render.
export const DEFAULT_SETTINGS: Settings = {
  globalShortcut: { control: true, alt: true, shift: false, meta: true, code: "KeyF" },
  defaultEntryPoint: { kind: "recents" },
  temporaryTabLifetime: { kind: "minutes", minutes: 180 },
  primaryActionDirectory: "enter",
  primaryActionFile: "open",
  afterAction: { ...DEFAULT_AFTER_ACTION },
  previewPanelVisible: true,
  terminalBundleId: null,
  editorBundleId: null,
  aliases: [],
  junkPatterns: [...DEFAULT_JUNK_PATTERNS],
  junkSeedVersion: CURRENT_JUNK_SEED_VERSION,
  // Assume dismissed until `get_settings` says otherwise, so the first-run banner never
  // flashes (and never fires `first_run_shown`) for a returning user before the load lands.
  // A genuine first run reports `firstRunDismissed: false` and the banner then appears (§13).
  firstRunDismissed: true,
};

// The Temporary Tab lifetime choices offered in Settings (SPEC §4): 30 m / 1 h / 3 h / 6 h /
// 12 h / 24 h / Never.
export const LIFETIME_OPTIONS: readonly LifetimeSetting[] = [
  { kind: "minutes", minutes: 30 },
  { kind: "minutes", minutes: 60 },
  { kind: "minutes", minutes: 180 },
  { kind: "minutes", minutes: 360 },
  { kind: "minutes", minutes: 720 },
  { kind: "minutes", minutes: 1440 },
  { kind: "never" },
];

// A stable key for a lifetime option, so a <select> can round-trip it without indices.
export function lifetimeKey(setting: LifetimeSetting): string {
  return setting.kind === "never" ? "never" : `m${String(setting.minutes)}`;
}

// A human label for the global shortcut, in macOS glyph order (SPEC §2 default `⌃⌥⌘F`).
export function shortcutLabel(spec: ShortcutSpec): string {
  const parts: string[] = [];
  if (spec.control) {
    parts.push("⌃");
  }
  if (spec.alt) {
    parts.push("⌥");
  }
  if (spec.shift) {
    parts.push("⇧");
  }
  if (spec.meta) {
    parts.push("⌘");
  }
  parts.push(keyLabel(spec.code));
  return parts.join("");
}

// A readable label for a `KeyboardEvent.code`: the letter/digit for `KeyF`/`Digit1`, else
// the raw code (`Slash`, `Space`, …) so an unusual capture is still legible.
function keyLabel(code: string): string {
  const letter = code.match(/^Key([A-Z])$/);
  if (letter?.[1] !== undefined) {
    return letter[1];
  }
  const digit = code.match(/^Digit([0-9])$/);
  if (digit?.[1] !== undefined) {
    return digit[1];
  }
  return code;
}

// A modifier-only key press (Control/Alt/Shift/Meta) that must not be captured as the
// shortcut's main key: the capture field ignores these and waits for a real key.
const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "ShiftLeft",
  "ShiftRight",
  "MetaLeft",
  "MetaRight",
]);

// Build a shortcut spec from a captured key event, or `null` when only a modifier was
// pressed (SPEC §2: the capture field records a full combo). A combo with no modifier is
// allowed but discouraged in the UI.
export function shortcutFromEvent(event: {
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}): ShortcutSpec | null {
  if (MODIFIER_CODES.has(event.code) || event.code === "") {
    return null;
  }
  return {
    control: event.ctrlKey,
    alt: event.altKey,
    shift: event.shiftKey,
    meta: event.metaKey,
    code: event.code,
  };
}
