import { app } from "electron";
import { appendFile, mkdir } from "node:fs/promises";
import path from "node:path";

const startedAt = process.hrtime.bigint();
let queue = Promise.resolve();

export function monotonicMs(): number {
  return Number(process.hrtime.bigint() - startedAt) / 1_000_000;
}

export function logPath(): string {
  return path.join(app.getPath("userData"), "events.ndjson");
}

export function logEvent(event: string, fields: Record<string, unknown> = {}): void {
  const record = JSON.stringify({
    event,
    monotonicMs: monotonicMs(),
    wallTime: new Date().toISOString(),
    pid: process.pid,
    ...fields,
  });
  const destination = logPath();
  queue = queue
    .then(async () => {
      await mkdir(path.dirname(destination), { recursive: true });
      await appendFile(destination, `${record}\n`, "utf8");
    })
    .catch((error) => console.error("Failed to write benchmark event", error));
}
