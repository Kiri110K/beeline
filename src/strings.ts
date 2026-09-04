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
  // The Preview Panel (SPEC §9): header labels for the Focused Item's metadata and the
  // fallbacks its lightweight body can show.
  preview: {
    empty: "No selection",
    kind: "Kind",
    size: "Size",
    created: "Created",
    modified: "Modified",
    itemsLabel: "Items",
    items: (count: number): string =>
      count === 1 ? "1 item" : `${String(count)} items`,
    // Appended to a text excerpt whose file continued past the byte budget (SPEC §9).
    truncationMark: "…",
    unavailable: "This item is no longer available",
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
  // The in-app Settings view (SPEC §12) and the one-time first-run guidance (SPEC §13).
  settings: {
    title: "Settings",
    close: "Close",
    done: "Done",
    shortcut: {
      label: "Global shortcut",
      hint: "Click and press a key combination",
      recording: "Press a combination…",
      edit: "Change",
      unavailable: "That combination is unavailable — the previous one is kept",
      noModifier: "Add at least one modifier (⌃ ⌥ ⇧ ⌘)",
    },
    entryPoint: {
      label: "Default entry point",
      recents: "Recents",
      directory: "Folder",
      pathPlaceholder: "/Users/…",
      // Short inline reasons a typed Folder path was rejected (SPEC §4): a domain sentence per
      // ListError code, never raw backend text.
      errors: {
        empty: "Enter a folder path",
        notFound: "That folder does not exist",
        notADirectory: "That path is a file, not a folder",
        permissionDenied: "No access to that folder",
        io: "That folder could not be opened",
      },
    },
    lifetime: {
      label: "Temporary tab lifetime",
      never: "Never",
      minutes: (minutes: number): string =>
        minutes % 60 === 0
          ? `${String(minutes / 60)} h`
          : `${String(minutes)} min`,
    },
    primaryAction: {
      directoryLabel: "When opening a folder",
      fileLabel: "When opening a file",
      enter: "Enter folder",
      open: "Open in default app",
      menu: "Show action menu",
    },
    learnedRanking: {
      heading: "Learned ranking",
      hint: "Clears Search Memory and learned usage. Recents, visit history, aliases, tabs, and the name index stay intact.",
      reset: "Reset learned ranking",
      confirm: "Confirm reset",
      cancel: "Cancel",
      running: "Resetting…",
      done: "Learned ranking reset",
    },
    afterAction: {
      heading: "After an action",
      hide: "Hide window",
      keep: "Keep open",
      labels: {
        open_file: "Open file",
        open_terminal: "Open in Terminal",
        open_editor: "Open in Editor",
        copy_path: "Copy Path",
        copy_file: "Copy File",
        trash: "Move to Trash",
        delete_permanently: "Delete Permanently",
        enter_directory: "Enter folder",
        navigation: "Navigation",
        paste: "Paste",
        move_paste: "Move Here",
        rename: "Rename",
        new_folder: "New Folder",
        reveal: "Reveal in Finder",
        open_in_new_tab: "Open in New Tab",
      },
    },
    preview: {
      label: "Preview panel",
      on: "Show the preview panel",
    },
    slots: {
      heading: "Applications",
      terminalLabel: "Terminal",
      editorLabel: "Editor",
      notInstalled: "not installed",
      none: "None detected",
    },
    aliases: {
      heading: "Aliases",
      hint: "A typed word recommends a folder in search — it never hides other results",
      wordPlaceholder: "word",
      pathPlaceholder: "/Users/…",
      add: "Add alias",
      remove: "Remove",
    },
    junk: {
      heading: "Junk patterns",
      hint: "Folder names classified as Junk: searchable, but ranked far down",
      placeholder: "folder name",
      add: "Add pattern",
      remove: "Remove",
      reset: "Reset to default",
    },
  },
  firstRun: {
    title: "Welcome to Beeline",
    body: "Beeline works best with Full Disk Access and starting at login, though both are optional. Full Disk Access is managed in macOS System Settings, and you can change the startup item later in macOS Login Items.",
    fullDiskAccess: "Grant Full Disk Access",
    loginItem: "Start at login",
    dismiss: "Dismiss",
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
