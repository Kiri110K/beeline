import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { loadSubmittedPath, loadRecents } from "../electron/filesystem";

const root = await mkdtemp(path.join(tmpdir(), "visual-files-electron-smoke-"));
await mkdir(path.join(root, "folder"));
await writeFile(path.join(root, "sample.txt"), "sample\n");

const directory = await loadSubmittedPath(root);
if (directory.items.length !== 2 || directory.items[0].kind !== "directory") throw new Error("Directory enumeration failed");

const file = await loadSubmittedPath(path.join(root, "sample.txt"));
if (file.selectPath !== path.join(root, "sample.txt")) throw new Error("File selection failed");

const recents = await loadRecents();
console.log(JSON.stringify({
  directoryItems: directory.items.map((item) => item.name),
  selectedFile: file.selectPath,
  recentsCount: recents.items.length,
  recentsDurationMs: Number(recents.durationMs.toFixed(2)),
}, null, 2));
