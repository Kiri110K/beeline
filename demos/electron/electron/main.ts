import { app, BrowserWindow, clipboard, globalShortcut, ipcMain, powerMonitor, shell } from "electron";
import { spawn, type ChildProcess } from "node:child_process";
import path from "node:path";
import { loadRecents, loadSubmittedPath } from "./filesystem";
import { shouldStartVisible, type ShowOrigin } from "./launch";
import { logEvent, logPath } from "./logger";

const DEFAULT_SHORTCUT = "CommandOrControl+Shift+F";
const shortcut = process.env.VISUAL_FILES_SHORTCUT || DEFAULT_SHORTCUT;
let mainWindow: BrowserWindow | null = null;
let quickLookProcess: ChildProcess | null = null;
let showSequence = 0;
let isQuitting = false;
let activeShowOrigin: ShowOrigin = "shortcut";
let testHookShown = false;
const startVisibleForTest = shouldStartVisible(process.env);

app.setName("Visual Files Electron Demo");
logEvent("process_start", { electron: process.versions.electron, chrome: process.versions.chrome });

function createWindow(): BrowserWindow {
  const window = new BrowserWindow({
    width: 920,
    height: 640,
    show: false,
    center: true,
    frame: false,
    type: "panel",
    backgroundColor: "#111216",
    roundedCorners: true,
    resizable: true,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  window.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true, skipTransformProcessType: true });
  window.on("close", (event) => {
    if (!isQuitting) {
      event.preventDefault();
      hideWindow("close-request");
    }
  });
  window.on("blur", () => {
    // Benchmark requires explicit Escape/shortcut dismissal, so blur does not hide.
  });
  window.webContents.on("render-process-gone", (_event, details) => {
    logEvent("renderer_gone", { reason: details.reason, exitCode: details.exitCode });
  });
  const entry = path.join(__dirname, "../dist/index.html");
  void window.loadFile(entry);
  return window;
}

function showWindow(origin: ShowOrigin): void {
  if (!mainWindow) return;
  const sequence = ++showSequence;
  activeShowOrigin = origin;
  logEvent("show_requested", { sequence, origin });
  app.focus({ steal: true });
  mainWindow.center();
  mainWindow.show();
  mainWindow.moveTop();
  mainWindow.focus();
  mainWindow.webContents.send("window:focus-path");
}

function hideWindow(reason: string): void {
  if (!mainWindow?.isVisible()) return;
  mainWindow.hide();
  logEvent("window_hidden", { reason });
}

function toggleWindow(): void {
  logEvent("shortcut_received", { shortcut });
  if (mainWindow?.isVisible()) hideWindow("shortcut");
  else showWindow("shortcut");
}

function registerShortcut(): boolean {
  globalShortcut.unregister(shortcut);
  const registered = globalShortcut.register(shortcut, toggleWindow);
  logEvent("shortcut_registration", { shortcut, registered });
  return registered;
}

function closeQuickLook(): void {
  if (quickLookProcess && !quickLookProcess.killed) quickLookProcess.kill("SIGTERM");
  quickLookProcess = null;
}

async function toggleQuickLook(itemPath: string | null): Promise<{ open: boolean; workaround: string }> {
  closeQuickLook();
  if (!itemPath) return { open: false, workaround: "qlmanage child process" };
  logEvent("quick_look_requested", { path: itemPath });
  quickLookProcess = spawn("/usr/bin/qlmanage", ["-p", itemPath], { stdio: "ignore" });
  quickLookProcess.once("exit", () => (quickLookProcess = null));
  return { open: true, workaround: "Electron has no Quick Look API; demo launches qlmanage and terminates it to toggle." };
}

app.whenReady().then(() => {
  if (process.platform === "darwin") app.dock?.hide();
  mainWindow = createWindow();
  const registered = registerShortcut();
  powerMonitor.on("resume", () => {
    logEvent("power_resume");
    if (!globalShortcut.isRegistered(shortcut)) registerShortcut();
  });
  logEvent("backend_ready", { shortcut, shortcutRegistered: registered, logPath: logPath() });
});

ipcMain.handle("filesystem:load-path", async (_event, input: string) => {
  logEvent("path_submitted", { input });
  const result = await loadSubmittedPath(input);
  logEvent("directory_loaded", { durationMs: result.durationMs, itemCount: result.items.length, location: result.location });
  return result;
});

ipcMain.handle("filesystem:recents", async () => {
  const result = await loadRecents();
  logEvent("recents_loaded", { durationMs: result.durationMs, itemCount: result.items.length });
  return result;
});

ipcMain.handle("action:open", async (_event, itemPath: string) => shell.openPath(itemPath));
ipcMain.handle("action:copy", (_event, itemPath: string) => clipboard.writeText(itemPath));
ipcMain.handle("action:quick-look", (_event, itemPath: string | null) => toggleQuickLook(itemPath));
ipcMain.handle("window:hide", () => hideWindow("escape"));
ipcMain.on("frontend:ready", () => {
  logEvent("frontend_ready", { startVisibleForTest });
  if (startVisibleForTest && !testHookShown) {
    testHookShown = true;
    showWindow("test-hook");
  }
});
ipcMain.on("frontend:visible-and-focused", (_event, focused: boolean) => {
  logEvent("window_visible", { focused, sequence: showSequence, origin: activeShowOrigin });
});
ipcMain.on("frontend:benchmark-rendered", (_event, count: number) => {
  logEvent("benchmark_10k_rendered", { itemCount: count });
});

app.on("window-all-closed", () => {
  // Stay resident on macOS. The hidden panel is recreated only on a fresh launch.
});
app.on("will-quit", () => {
  closeQuickLook();
  globalShortcut.unregisterAll();
});
app.on("before-quit", () => {
  isQuitting = true;
});
