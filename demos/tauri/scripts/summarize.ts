#!/usr/bin/env bun

type EventRecord = {
  event: string;
  monotonic_ms: number;
  duration_ms?: number;
  item_count?: number;
  [key: string]: unknown;
};

const defaultPath = `${process.env.HOME}/Library/Application Support/com.kiri.visual-files-tauri-benchmark/benchmark.ndjson`;
const logPath = process.argv[2] ?? defaultPath;
const file = Bun.file(logPath);

if (!(await file.exists())) {
  console.error(`No event log at ${logPath}`);
  process.exit(1);
}

const records = (await file.text())
  .split("\n")
  .filter(Boolean)
  .map((line, index) => {
    try {
      return JSON.parse(line) as EventRecord;
    } catch (error) {
      throw new Error(`Invalid JSON on line ${index + 1}: ${error}`);
    }
  });

function quantile(values: number[], fraction: number) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)];
}

function stats(values: number[]) {
  if (!values.length) return "no samples";
  return `n=${values.length} median=${quantile(values, 0.5)!.toFixed(2)}ms p95=${quantile(values, 0.95)!.toFixed(2)}ms slowest=${Math.max(...values).toFixed(2)}ms`;
}

const shortcutToVisible: number[] = [];
let pendingShortcut: number | null = null;
for (const record of records) {
  if (record.event === "shortcut_received") pendingShortcut = record.monotonic_ms;
  if (record.event === "window_visible" && pendingShortcut != null) {
    shortcutToVisible.push(record.monotonic_ms - pendingShortcut);
    pendingShortcut = null;
  }
}

const directoryLoads = records.flatMap((record) =>
  record.event === "directory_loaded" && typeof record.duration_ms === "number" ? [record.duration_ms] : [],
);
const recentsLoads = records.flatMap((record) =>
  record.event === "recents_loaded" && typeof record.duration_ms === "number" ? [record.duration_ms] : [],
);
const eventCounts = new Map<string, number>();
for (const record of records) eventCounts.set(record.event, (eventCounts.get(record.event) ?? 0) + 1);

console.log(`log: ${logPath}`);
console.log(`records: ${records.length}`);
console.log(`shortcut_received -> window_visible: ${stats(shortcutToVisible)}`);
console.log(`directory_loaded duration: ${stats(directoryLoads)}`);
console.log(`recents_loaded duration: ${stats(recentsLoads)}`);
console.log("events:");
for (const [event, count] of [...eventCounts].sort(([left], [right]) => left.localeCompare(right))) {
  console.log(`  ${event}: ${count}`);
}
