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
