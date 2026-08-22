import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";

interface EventRecord { event: string; monotonicMs: number; pid: number; [key: string]: unknown }

const defaultPath = path.join(homedir(), "Library/Application Support/Visual Files Electron Demo/events.ndjson");
const logPath = process.argv[2] || defaultPath;
if (!existsSync(logPath)) {
  console.error(`No event log at ${logPath}`);
  process.exit(1);
}

const events = readFileSync(logPath, "utf8").trim().split("\n").filter(Boolean).map((line) => JSON.parse(line) as EventRecord);

function stats(values: number[]) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const percentile = (value: number) => sorted[Math.min(sorted.length - 1, Math.ceil(value * sorted.length) - 1)];
  return {
    samples: values.length,
    medianMs: Number(percentile(0.5).toFixed(2)),
    p95Ms: Number(percentile(0.95).toFixed(2)),
    slowestMs: Number(sorted.at(-1)!.toFixed(2)),
  };
}

const shows: number[] = [];
const pendingShows = new Map<number, number>();
for (const event of events) {
  if (event.event === "show_requested" && typeof event.sequence === "number") pendingShows.set(event.sequence, event.monotonicMs);
  if (event.event === "window_visible" && typeof event.sequence === "number") {
    const start = pendingShows.get(event.sequence);
    if (start !== undefined) shows.push(event.monotonicMs - start);
  }
}

const byDuration = (name: string) => events
  .filter((event) => event.event === name && typeof event.durationMs === "number")
  .map((event) => event.durationMs as number);

console.log(JSON.stringify({
  logPath,
  processCount: new Set(events.map((event) => event.pid)).size,
  eventCount: events.length,
  warmShowToFocused: stats(shows),
  directoryLoads: stats(byDuration("directory_loaded")),
  recentsLoads: stats(byDuration("recents_loaded")),
  focusFailures: events.filter((event) => event.event === "window_visible" && event.focused === false).length,
  rendererFailures: events.filter((event) => event.event === "renderer_gone").length,
}, null, 2));
