// Central English strings. Kept out of components so localization stays possible
// (SPEC §1: no hard-coded strings inside components).
export const strings = {
  app: { title: "Beeline" },
  navigation: {
    searchLabel: "Search",
    placeholder: "Search files or type a path",
  },
  columns: { name: "Name", kind: "Kind", modified: "Modified", size: "Size" },
  rowState: {
    empty: "This folder is empty",
    notFound: (path: string): string => `Not found — ${path}`,
    noAccess: (path: string): string => `No access — ${path}`,
  },
  recents: {
    // Honest empty vs Spotlight-unavailable, one explanatory line each (SPEC §7).
    empty: "No recent files",
    unavailable: {
      disabled: "Recents is unavailable — Spotlight indexing is turned off",
      privacy_excluded:
        "Recents is unavailable — this location is excluded from Spotlight",
      indexing: "Recents is updating — Spotlight is still indexing",
      unknown: "Recents is unavailable",
    },
  },
  search: {
    // Row-state lines, never banners or raw backend text (SPEC §6).
    empty: "Nothing found",
    searching: "Searching…",
    failed: "Search is unavailable",
    tier: { hidden: "hidden", junk: "junk" },
  },
  zones: {
    tabStrip: "Tabs",
    previewPanel: "Preview panel",
    statusStrip: "Status",
  },
  operations: {
    // The one Action Menu (SPEC §5): item-scope file actions and app-scope actions.
    menu: {
      open: "Open",
      quickLook: "Quick Look",
      copyPath: "Copy Path",
      copyFile: "Copy File",
      paste: "Paste",
      movePaste: "Move Here",
      rename: "Rename",
      newFolder: "New Folder",
      trash: "Move to Trash",
      deletePermanently: "Delete Permanently…",
      reveal: "Reveal in Finder",
      openTerminal: "Open in Terminal",
      openEditor: "Open in Editor",
      openInNewTab: "Open in New Tab",
      newTab: "New Tab",
      pastePath: "Paste Path",
      refresh: "Refresh",
      settings: "Settings",
      quit: "Quit",
    },
    rowMenuLabel: "Actions",
    renamePlaceholder: "New name",
    newFolderName: "untitled folder",
    // Concrete Status Strip / inline sentences for a pre-dispatch failure (SPEC §13).
    // A rename collision uses `nameCollision` inline in the row.
    errors: {
      nameCollision: "A file with that name already exists",
      invalidName: (reason: string): string => `Invalid name — ${reason}`,
      sourceNotFound: (path: string): string => `No longer exists — ${path}`,
      targetNotFound: (path: string): string => `Folder no longer exists — ${path}`,
      targetNotADirectory: (path: string): string => `Not a folder — ${path}`,
    },
    // Status Strip labels (SPEC §8, §10, §13): batch job labels and problem-list controls.
    status: {
      copying: "Copying",
      moving: "Moving",
      trashing: "Moving to Trash",
      deleting: "Deleting",
      dismiss: "Dismiss",
      noTerminal: "No Terminal application is installed",
      noEditor: "No editor application is installed",
    },
    // The always-on confirm for Delete Permanently (SPEC §8: no "don't ask again").
    confirm: {
      message: (count: number): string =>
        count === 1
          ? "Delete 1 item permanently? This cannot be undone."
          : `Delete ${String(count)} items permanently? This cannot be undone.`,
      confirm: "Delete",
      cancel: "Cancel",
    },
  },
  tabs: {
    untitled: "Untitled",
    recents: "Recents",
    newTab: "New tab",
    renamePlaceholder: "Tab name",
    menu: {
      pin: "Pin Tab",
      close: "Close Tab",
      rename: "Rename",
      unpin: "Unpin Tab",
      remove: "Remove Pinned Tab",
      copyLocation: "Copy Location",
    },
  },
};
