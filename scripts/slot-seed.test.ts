// Regression check for the pure Terminal/Editor slot seed-merge helper (SPEC §8, §12).
//
// `seededSlotUpdate` decides the one combined Settings update first-run auto-seeding writes:
// fill an empty slot with its detected candidate, never overwrite a slot already filled (by the
// user or a write that landed while detection was in flight), leave an empty slot empty when no
// candidate is installed, and report `null` when nothing changes so no save fires.
//
// Like the other checks it loads the *real* `src/operations/settings.ts` through Vite's SSR
// loader, exercising the shipped helper with no new dependency. Any assertion failure exits
// non-zero.

import assert from "node:assert/strict";

import { createServer } from "vite";
import { z } from "zod";

const moduleShape = z.object({
  seededSlotUpdate: z.custom<
    (
      current: { terminalBundleId: string | null; editorBundleId: string | null },
      resolved: { terminalBundleId: string | null; editorBundleId: string | null },
    ) => { terminalBundleId: string | null; editorBundleId: string | null } | null
  >((value) => typeof value === "function"),
});

async function loadModule(): Promise<z.infer<typeof moduleShape>> {
  const server = await createServer({
    configFile: false,
    logLevel: "silent",
    server: { middlewareMode: true },
    appType: "custom",
  });
  try {
    const raw: unknown = await server.ssrLoadModule("/src/operations/settings.ts");
    return moduleShape.parse(raw);
  } finally {
    await server.close();
  }
}

const { seededSlotUpdate } = await loadModule();

// 1. Both slots empty, both candidates detected: one update fills both.
assert.deepEqual(
  seededSlotUpdate(
    { terminalBundleId: null, editorBundleId: null },
    { terminalBundleId: "com.mitchellh.ghostty", editorBundleId: "dev.zed.Zed" },
  ),
  { terminalBundleId: "com.mitchellh.ghostty", editorBundleId: "dev.zed.Zed" },
  "two empty slots with detected candidates must be seeded together",
);

// 2. No candidate installed for either empty slot: nothing changes, so no save.
assert.equal(
  seededSlotUpdate(
    { terminalBundleId: null, editorBundleId: null },
    { terminalBundleId: null, editorBundleId: null },
  ),
  null,
  "empty slots with no installed candidate must report no update",
);

// 3. A slot filled during detection is preserved, never overwritten by a candidate.
assert.deepEqual(
  seededSlotUpdate(
    { terminalBundleId: "com.googlecode.iterm2", editorBundleId: null },
    { terminalBundleId: "com.mitchellh.ghostty", editorBundleId: "dev.zed.Zed" },
  ),
  { terminalBundleId: "com.googlecode.iterm2", editorBundleId: "dev.zed.Zed" },
  "a slot already filled must survive while the other empty slot is seeded",
);

// 4. Both slots already configured: no update even if candidates resolve.
assert.equal(
  seededSlotUpdate(
    { terminalBundleId: "com.apple.Terminal", editorBundleId: "com.microsoft.VSCode" },
    { terminalBundleId: "com.mitchellh.ghostty", editorBundleId: "dev.zed.Zed" },
  ),
  null,
  "two configured slots must never be overwritten by auto-seeding",
);

// 5. One empty slot with no candidate, the other configured: still no change.
assert.equal(
  seededSlotUpdate(
    { terminalBundleId: "com.apple.Terminal", editorBundleId: null },
    { terminalBundleId: null, editorBundleId: null },
  ),
  null,
  "an empty slot with no candidate beside a configured slot must report no update",
);

console.log("slot seed: OK");
