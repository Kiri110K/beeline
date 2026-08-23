// Central English strings. Kept out of components so localization stays possible
// (SPEC §1: no hard-coded strings inside components).
export const strings = {
  app: { title: "Beeline" },
  navigation: { inputLabel: "Current location" },
  columns: { name: "Name", kind: "Kind", modified: "Modified", size: "Size" },
  rowState: {
    empty: "This folder is empty",
    notFound: (path: string): string => `Not found — ${path}`,
    noAccess: (path: string): string => `No access — ${path}`,
  },
  zones: {
    tabStrip: "Tabs",
    previewPanel: "Preview panel",
    statusStrip: "Status",
  },
  tabs: {
    untitled: "Untitled",
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
