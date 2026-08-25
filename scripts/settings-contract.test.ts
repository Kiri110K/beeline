// Contract regression check for the frontend/backend Settings boundary (SPEC §12).
//
// The Rust `AppSettings` serializes an internal `junkSeedVersion` field (settings.rs). The
// strict frontend zod parser must accept a full backend-shaped payload including that field,
// carry the exact version through unchanged (so `set_settings` round-trips it and the
// Junk-seed migration never re-runs), and still reject genuinely unknown fields.
//
// This runs the *real* `src/settings/schema.ts` through Vite's SSR loader, so it resolves the
// app's extensionless imports and exercises the same parser the app ships — no new dependency,
// only the existing Vite/Node toolchain. Any assertion failure exits non-zero.

import assert from "node:assert/strict";

import { createServer } from "vite";
import { z } from "zod";

// The slice of `schema.ts` this check consumes, validated at the module boundary so the loaded
// module is typed without an unchecked `any` (docs/agents/typescript.md: parse, don't validate).
const moduleShape = z.object({
  settingsSchema: z.instanceof(z.ZodType),
  DEFAULT_SETTINGS: z.unknown(),
  DEFAULT_JUNK_PATTERNS: z.array(z.string()),
  CURRENT_JUNK_SEED_VERSION: z.number(),
});

// A view over a parsed Settings value: only the fields this check inspects.
const settingsView = z.object({
  junkSeedVersion: z.number(),
  junkPatterns: z.array(z.string()),
});

// A backend-shaped `get_settings` response (camelCase, field-for-field with Rust serde output),
// including `junkSeedVersion`. `junkSeedVersion` is a non-default value so a survived version
// proves a genuine carry rather than a re-defaulted one; `junkPatterns` omits the built-in
// "Logs" to model a user who deleted a built-in after the Junk-seed migration.
const backendPayload = {
  globalShortcut: { control: true, alt: true, shift: false, meta: true, code: "KeyF" },
  defaultEntryPoint: { kind: "recents" },
  temporaryTabLifetime: { kind: "minutes", minutes: 180 },
  primaryActionDirectory: "enter",
  primaryActionFile: "open",
  afterAction: { open_file: "hide", trash: "keep" },
  previewPanelVisible: true,
  terminalBundleId: null,
  editorBundleId: null,
  aliases: [{ word: "docs", path: "~/Documents" }],
  junkPatterns: ["node_modules", "Containers"],
  junkSeedVersion: 7,
  firstRunDismissed: true,
};

async function loadSchemaModule(): Promise<z.infer<typeof moduleShape>> {
  const server = await createServer({
    configFile: false,
    logLevel: "silent",
    server: { middlewareMode: true },
    appType: "custom",
  });
  try {
    const raw: unknown = await server.ssrLoadModule("/src/settings/schema.ts");
    return moduleShape.parse(raw);
  } finally {
    await server.close();
  }
}

const schema = await loadSchemaModule();

// 1. The strict parser accepts the full backend payload, junkSeedVersion and all.
const parsed = settingsView.parse(schema.settingsSchema.parse(backendPayload));

// 2. The exact internal seed version survives the parse, so it round-trips to set_settings.
assert.equal(
  parsed.junkSeedVersion,
  7,
  "junkSeedVersion must survive parsing so it round-trips back to set_settings",
);

// 3. A user's deletion of a built-in Junk pattern survives: the parser adds nothing back.
assert.ok(
  !parsed.junkPatterns.includes("Logs"),
  "parsing must not re-inject a built-in Junk pattern the user deleted",
);

// 4. junkSeedVersion is required, so the frontend can never drop it and force a re-migration.
const withoutSeedVersion: Record<string, unknown> = { ...backendPayload };
delete withoutSeedVersion["junkSeedVersion"];
assert.throws(
  () => schema.settingsSchema.parse(withoutSeedVersion),
  "a payload missing junkSeedVersion must be rejected",
);

// 5. Strictness is intact: a genuinely unknown field is still rejected.
assert.throws(
  () => schema.settingsSchema.parse({ ...backendPayload, bogusField: 1 }),
  "an unknown field must still be rejected by the strict schema",
);

// 6. The pre-load fallback defaults parse cleanly and carry the current seed version.
const defaults = settingsView.parse(schema.settingsSchema.parse(schema.DEFAULT_SETTINGS));
assert.equal(
  defaults.junkSeedVersion,
  schema.CURRENT_JUNK_SEED_VERSION,
  "DEFAULT_SETTINGS must carry the current Junk seed version",
);

// 7. The frontend Junk defaults include the seed-v2 built-ins, matching the Rust seed list.
for (const name of ["Containers", "Group Containers", "Application Support", "Logs", "Saved Application State"]) {
  assert.ok(
    schema.DEFAULT_JUNK_PATTERNS.includes(name),
    `DEFAULT_JUNK_PATTERNS must include the seed-v2 built-in "${name}"`,
  );
}

console.log("settings contract: OK");
