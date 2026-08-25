import type { SetSettingsOutcome } from "./ipc";
import type { Settings } from "./schema";

// The user-facing fields whose change is worth a `settings_changed {field}` event (SPEC §12
// telemetry point; the field name only, never its value). `firstRunDismissed` and
// `junkSeedVersion` are internal persistence (§12–§13), never user settings, and excluded —
// dismissal has its own `first_run_dismissed` event.
export const TRACKED_FIELDS = [
  "globalShortcut",
  "defaultEntryPoint",
  "temporaryTabLifetime",
  "primaryActionDirectory",
  "primaryActionFile",
  "afterAction",
  "previewPanelVisible",
  "terminalBundleId",
  "editorBundleId",
  "aliases",
  "junkPatterns",
] as const;
export type TrackedField = (typeof TRACKED_FIELDS)[number];

// Pure: the tracked fields that actually changed once the backend's write outcome is applied.
// The store fires `settings_changed` only for these, and only after a confirmed save, so a
// telemetry event always reflects a real persisted change (SPEC §12). A rejected global
// shortcut (`shortcutRegistered: false`) means the backend kept the previous shortcut, so
// `globalShortcut` did not change even though the draft proposed a new one (SPEC §2) — it is
// compared against `prev`, not the rejected `next`, and so never reported.
export function changedTrackedFields(
  prev: Settings,
  next: Settings,
  outcome: SetSettingsOutcome,
): readonly TrackedField[] {
  const effective: Settings = outcome.shortcutRegistered
    ? next
    : { ...next, globalShortcut: prev.globalShortcut };
  return TRACKED_FIELDS.filter(
    (field) => JSON.stringify(prev[field]) !== JSON.stringify(effective[field]),
  );
}
