import { contextBridge, ipcRenderer } from "electron";
import type { LocationResult, RecentResult } from "./types";

contextBridge.exposeInMainWorld("visualFiles", {
  loadPath: (input: string): Promise<LocationResult> => ipcRenderer.invoke("filesystem:load-path", input),
  loadRecents: (): Promise<RecentResult> => ipcRenderer.invoke("filesystem:recents"),
  openPath: (input: string): Promise<string> => ipcRenderer.invoke("action:open", input),
  copyPath: (input: string): Promise<void> => ipcRenderer.invoke("action:copy", input),
  quickLook: (input: string | null): Promise<{ open: boolean; workaround: string }> =>
    ipcRenderer.invoke("action:quick-look", input),
  hideWindow: (): Promise<void> => ipcRenderer.invoke("window:hide"),
  frontendReady: (): void => ipcRenderer.send("frontend:ready"),
  visibleAndFocused: (focused: boolean): void => ipcRenderer.send("frontend:visible-and-focused", focused),
  benchmarkRendered: (count: number): void => ipcRenderer.send("frontend:benchmark-rendered", count),
  onFocusPath: (callback: () => void): (() => void) => {
    const listener = () => callback();
    ipcRenderer.on("window:focus-path", listener);
    return () => ipcRenderer.removeListener("window:focus-path", listener);
  },
});
