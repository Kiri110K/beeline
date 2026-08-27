// Regression check for issue #38. A directory can expose 50,000 rows while the Browse state
// retains only one metadata window. Focus and Selected Item paths must survive replacement of
// that window, because Tab history and file operations use them after rows scroll away.

import assert from "node:assert/strict";

import { createServer } from "vite";
import { z } from "zod";

const reducerModuleSchema = z.object({
  browseReducer: z.custom<(state: unknown, action: unknown) => unknown>(
    (value) => typeof value === "function",
  ),
  initialBrowseState: z.unknown(),
  selectedPathsOf: z.custom<(state: unknown) => unknown>(
    (value) => typeof value === "function",
  ),
});

const readyStateSchema = z.object({
  load: z.object({
    status: z.literal("ready"),
    items: z.array(z.looseObject({ path: z.string() })),
    offset: z.number(),
    total: z.number(),
    sessionId: z.string().nullable(),
  }),
  focusedIndex: z.number(),
  focusedPath: z.string().nullable(),
  selected: z.set(z.number()),
  selectedPaths: z.map(z.number(), z.string()),
});

function item(index: number): Record<string, unknown> {
  const name = `file_${index.toString().padStart(5, "0")}.txt`;
  return {
    name,
    path: `/fixture/${name}`,
    isDirectory: false,
    isHidden: false,
    kind: "TXT",
    modifiedMs: 1,
    sizeBytes: 1,
  };
}

const server = await createServer({
  configFile: false,
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const raw: unknown = await server.ssrLoadModule("/src/browse/state.ts");
  const module = reducerModuleSchema.parse(raw);
  const targetIndex = 42_000;
  const targetPath = item(targetIndex)["path"];
  assert.equal(typeof targetPath, "string");

  const prefixOffset = targetIndex - 32;
  const prefix = Array.from({ length: 64 }, (_, offset) =>
    item(prefixOffset + offset),
  );
  const listed = module.browseReducer(module.initialBrowseState, {
    type: "listed",
    location: { kind: "directory", path: "/fixture" },
    result: {
      kind: "items",
      load: {
        items: prefix,
        offset: prefixOffset,
        total: 50_000,
        sessionId: "session-1",
        focusIndex: targetIndex,
        selected: [{ path: targetPath, index: targetIndex }],
      },
    },
    nav: "replace",
    originScrollTop: 0,
    focusPath: targetPath,
  });
  const initial = readyStateSchema.parse(listed);
  assert.equal(initial.load.items.length, 64);
  assert.equal(initial.load.total, 50_000);
  assert.equal(initial.focusedIndex, targetIndex);
  assert.equal(initial.focusedPath, targetPath);

  const windowItems = Array.from({ length: 2_048 }, (_, index) => item(index));
  const shifted = module.browseReducer(listed, {
    type: "windowLoaded",
    sessionId: "session-1",
    items: windowItems,
    offset: 0,
    total: 50_000,
  });
  const after = readyStateSchema.parse(shifted);

  assert.equal(after.load.items.length, 2_048);
  assert.equal(after.load.total, 50_000);
  assert.equal(after.focusedIndex, targetIndex);
  assert.equal(after.focusedPath, targetPath);
  assert.deepEqual(z.array(z.string()).parse(module.selectedPathsOf(shifted)), [targetPath]);
} finally {
  await server.close();
}

console.log("browse metadata window: OK");
