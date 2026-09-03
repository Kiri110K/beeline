// Search v2 result-stream contract: waves may reorder every row except the deliberately
// Focused Item. Query changes clear that preservation, and stale waves are ignored.

import assert from "node:assert/strict";

import { createServer } from "vite";
import { z } from "zod";

const moduleSchema = z.object({
  initialSearchState: z.unknown(),
  searchReducer: z.custom<(state: unknown, action: unknown) => unknown>(
    (value) => typeof value === "function",
  ),
});

const hit = (path: string): Record<string, unknown> => ({
  name: path.slice(1),
  path,
  isDirectory: false,
  tier: "normal",
});

const wave = (
  stage: "working_set" | "exact" | "fuzzy",
  paths: string[],
  complete = false,
): Record<string, unknown> => ({
  stage,
  revision: 1,
  complete,
  qgramReady: true,
  scanned: paths.length,
  backendDurationMs: 1,
  hits: paths.map(hit),
});

const server = await createServer({
  configFile: false,
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const loaded: unknown = await server.ssrLoadModule("/src/search/state.ts");
  const { initialSearchState, searchReducer } = moduleSchema.parse(loaded);
  let state = searchReducer(initialSearchState, { type: "activate" });
  state = searchReducer(state, { type: "queryChanged", query: "methodolgy" });
  state = searchReducer(state, {
    type: "resultsWave",
    query: "methodolgy",
    wave: wave("working_set", ["/a", "/b"]),
  });
  state = searchReducer(state, { type: "focusDelta", delta: 1 });
  state = searchReducer(state, {
    type: "resultsWave",
    query: "methodolgy",
    wave: wave("exact", ["/c", "/b", "/a"]),
  });
  assert.equal(
    z.object({ focusedIndex: z.number(), deliberateFocusPath: z.string() }).parse(state)
      .focusedIndex,
    1,
    "the explicitly Focused Item must survive the exact wave by identity",
  );
  state = searchReducer(state, {
    type: "resultsWave",
    query: "methodolgy",
    wave: wave("fuzzy", ["/d", "/c", "/b", "/a"], true),
  });
  assert.deepEqual(
    z
      .object({
        focusedIndex: z.number(),
        deliberateFocusPath: z.string(),
        results: z.object({ status: z.literal("done") }),
      })
      .parse(state),
    {
      focusedIndex: 2,
      deliberateFocusPath: "/b",
      results: { status: "done" },
    },
    "later fuzzy ranking may move the Focused Item but may not replace it",
  );

  state = searchReducer(state, { type: "queryChanged", query: "methodology" });
  const changed = z
    .object({ focusedIndex: z.number(), deliberateFocusPath: z.null() })
    .parse(state);
  assert.equal(changed.focusedIndex, 0, "a query change restores automatic first-row focus");

  const beforeStale = state;
  state = searchReducer(state, {
    type: "resultsWave",
    query: "methodolgy",
    wave: wave("fuzzy", ["/stale"], true),
  });
  assert.equal(state, beforeStale, "a wave for an older query must be ignored");
} finally {
  await server.close();
}

console.log("search stream contract: ok");
