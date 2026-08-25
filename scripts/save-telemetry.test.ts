// Regression check for the pure save-telemetry logic (SPEC §2, §12).
//
// `changedTrackedFields` decides which `settings_changed {field}` events the store fires
// after a *confirmed* write. Two properties matter: only genuinely changed tracked fields are
// reported, and a global shortcut the backend rejected (`shortcutRegistered: false`, kept the
// previous one) is never reported as changed even though the draft proposed a new value.
//
// Like the settings-contract check, this loads the *real* module through Vite's SSR loader so
// it resolves the app's extensionless, type-only imports and exercises the shipped code — no
// new dependency, only the existing Vite/Node toolchain. Any assertion failure exits non-zero.

import assert from "node:assert/strict";

import { createServer } from "vite";
import { z } from "zod";

const moduleShape = z.object({
  TRACKED_FIELDS: z.array(z.string()),
  changedTrackedFields: z.custom<
    (prev: unknown, next: unknown, outcome: { shortcutRegistered: boolean }) => string[]
  >((value) => typeof value === "function"),
});

const schemaShape = z.object({
  DEFAULT_SETTINGS: z.record(z.string(), z.unknown()),
});

async function loadModules(): Promise<{
  save: z.infer<typeof moduleShape>;
  schema: z.infer<typeof schemaShape>;
}> {
  const server = await createServer({
    configFile: false,
    logLevel: "silent",
    server: { middlewareMode: true },
    appType: "custom",
  });
  try {
    const rawSave: unknown = await server.ssrLoadModule("/src/settings/saveTelemetry.ts");
    const rawSchema: unknown = await server.ssrLoadModule("/src/settings/schema.ts");
    return { save: moduleShape.parse(rawSave), schema: schemaShape.parse(rawSchema) };
  } finally {
    await server.close();
  }
}

const { save, schema } = await loadModules();
const { changedTrackedFields } = save;

// A confirmed write with the shortcut accepted; the base is the shipped default settings.
const okOutcome = { shortcutRegistered: true };
const rejectedOutcome = { shortcutRegistered: false };
const base = schema.DEFAULT_SETTINGS;

// 1. No change at all: nothing is reported.
assert.deepEqual(
  changedTrackedFields(base, base, okOutcome),
  [],
  "an unchanged save must report no settings_changed fields",
);

// 2. A single tracked field flips: only that field is reported.
const previewOff = { ...base, previewPanelVisible: false };
assert.deepEqual(
  changedTrackedFields(base, previewOff, okOutcome),
  ["previewPanelVisible"],
  "flipping previewPanelVisible must report exactly that field",
);

// 3. An accepted shortcut change is reported.
const newShortcut = { control: false, alt: false, shift: true, meta: true, code: "KeyG" };
const shortcutChanged = { ...base, globalShortcut: newShortcut };
assert.deepEqual(
  changedTrackedFields(base, shortcutChanged, okOutcome),
  ["globalShortcut"],
  "an accepted shortcut change must report globalShortcut",
);

// 4. A rejected shortcut is NOT reported — the backend kept the previous one — while a real
//    change made in the same write still is.
const rejectedButPreviewToo = {
  ...base,
  globalShortcut: newShortcut,
  previewPanelVisible: false,
};
assert.deepEqual(
  changedTrackedFields(base, rejectedButPreviewToo, rejectedOutcome),
  ["previewPanelVisible"],
  "a rejected shortcut must not be reported, but a co-changed field still is",
);

// 5. Internal persistence (firstRunDismissed) is never a tracked field.
const dismissedFlipped = { ...base, firstRunDismissed: false };
assert.deepEqual(
  changedTrackedFields(base, dismissedFlipped, okOutcome),
  [],
  "flipping the internal firstRunDismissed flag must report nothing",
);

// 6. TRACKED_FIELDS covers only user-facing settings, never internal persistence.
for (const internal of ["firstRunDismissed", "junkSeedVersion"]) {
  assert.ok(
    !save.TRACKED_FIELDS.includes(internal),
    `TRACKED_FIELDS must exclude the internal field "${internal}"`,
  );
}

console.log("save telemetry: OK");
