import { spawn } from "node:child_process";
import { homedir } from "node:os";
import path from "node:path";
import { readdir, realpath, stat } from "node:fs/promises";
import type { Dirent } from "node:fs";
import type { FileItem, LocationResult, RecentResult } from "./types";

const STAT_CONCURRENCY = 32;
const RECENTS_LIMIT = 300;

export function expandPath(input: string): string {
  const trimmed = input.trim();
  if (trimmed === "~") return homedir();
  if (trimmed.startsWith("~/")) return path.join(homedir(), trimmed.slice(2));
  return path.resolve(trimmed);
}

function kindFor(entry: Dirent): FileItem["kind"] {
  if (entry.isDirectory()) return "directory";
  if (entry.isFile()) return "file";
  return "other";
}

async function mapWithConcurrency<T, R>(
  values: T[],
  concurrency: number,
  mapper: (value: T, index: number) => Promise<R>,
): Promise<R[]> {
  const output = new Array<R>(values.length);
  let cursor = 0;
  const workers = Array.from({ length: Math.min(concurrency, values.length) }, async () => {
    while (cursor < values.length) {
      const index = cursor++;
      output[index] = await mapper(values[index], index);
    }
  });
  await Promise.all(workers);
  return output;
}

async function itemFromEntry(directory: string, entry: Dirent): Promise<FileItem> {
  const itemPath = path.join(directory, entry.name);
  try {
    const metadata = await stat(itemPath);
    return {
      id: itemPath,
      path: itemPath,
      name: entry.name,
      kind: kindFor(entry),
      modifiedMs: metadata.mtimeMs,
      size: entry.isFile() ? metadata.size : null,
    };
  } catch {
    return {
      id: itemPath,
      path: itemPath,
      name: entry.name,
      kind: kindFor(entry),
      modifiedMs: null,
      size: null,
    };
  }
}

export async function loadSubmittedPath(input: string): Promise<LocationResult> {
  const start = performance.now();
  const expanded = expandPath(input);
  const metadata = await stat(expanded);
  const directory = metadata.isDirectory() ? expanded : path.dirname(expanded);
  const selectPath = metadata.isDirectory() ? null : expanded;
  const canonical = await realpath(directory).catch(() => directory);
  const entries = await readdir(canonical, { withFileTypes: true });
  const items = await mapWithConcurrency(entries, STAT_CONCURRENCY, (entry) => itemFromEntry(canonical, entry));
  items.sort((a, b) => {
    if (a.kind === "directory" && b.kind !== "directory") return -1;
    if (a.kind !== "directory" && b.kind === "directory") return 1;
    return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
  });
  return { location: canonical, selectPath, items, durationMs: performance.now() - start };
}

function runMdfind(): Promise<{ paths: string[]; warning?: string }> {
  return new Promise((resolve, reject) => {
    const query = "kMDItemLastUsedDate >= $time.today(-90)";
    const child = spawn("/usr/bin/mdfind", [query], { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      if (stdout.length < 4_000_000) stdout += chunk;
    });
    child.stderr.on("data", (chunk: string) => (stderr += chunk));
    child.once("error", reject);
    child.once("close", (code) => {
      if (code !== 0) reject(new Error(`mdfind exited with ${code}: ${stderr.trim()}`));
      else resolve({
        paths: stdout.split("\n").filter(Boolean).slice(0, RECENTS_LIMIT),
        warning: stderr.trim() || undefined,
      });
    });
  });
}

async function itemFromPath(itemPath: string): Promise<FileItem | null> {
  try {
    const metadata = await stat(itemPath);
    return {
      id: itemPath,
      path: itemPath,
      name: path.basename(itemPath) || itemPath,
      kind: metadata.isDirectory() ? "directory" : metadata.isFile() ? "file" : "other",
      modifiedMs: metadata.mtimeMs,
      size: metadata.isFile() ? metadata.size : null,
    };
  } catch {
    return null;
  }
}

export async function loadRecents(): Promise<RecentResult> {
  const start = performance.now();
  const search = await runMdfind();
  const mapped = await mapWithConcurrency(search.paths, STAT_CONCURRENCY, (itemPath) => itemFromPath(itemPath));
  const items = mapped.filter((item): item is FileItem => item !== null);
  items.sort((a, b) => (b.modifiedMs ?? 0) - (a.modifiedMs ?? 0));
  return {
    items,
    durationMs: performance.now() - start,
    warning: search.warning
      ? `Spotlight reported: ${search.warning}`
      : "Spotlight Recents approximate Finder Recents; Finder's private filters are unavailable.",
  };
}
