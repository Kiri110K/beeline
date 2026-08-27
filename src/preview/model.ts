import { itemAt, type BrowseState } from "../browse/state";
import { displayedHits, type SearchState } from "../search/state";
import type { Settings } from "../settings/schema";

// The Focused Item the Preview Panel and Quick Look act on, reduced to what both a directory
// listing `Item` and a `SearchHit` can supply (SPEC §9: the panel follows the Focused Item in
// Browse and in Search Results). Name is always known, so the header paints it immediately;
// size / kind / timestamps arrive from `preview_metadata`.
export interface PreviewTarget {
  path: string;
  name: string;
  isDirectory: boolean;
}

// The Focused Item of the active Tab: the focused Search Result while Search Mode is up,
// otherwise the focused row of a ready Browse listing. `null` when nothing is focused (an
// empty listing, or a Search overlay with no results).
export function previewTargetFor(
  browse: BrowseState,
  search: SearchState,
): PreviewTarget | null {
  if (search.mode === "search") {
    const hit = displayedHits(search.results)[search.focusedIndex];
    if (hit === undefined) {
      return null;
    }
    return { path: hit.path, name: hit.name, isDirectory: hit.isDirectory };
  }
  if (browse.load.status === "ready") {
    const item = itemAt(browse.load, browse.focusedIndex);
    if (item !== undefined) {
      return { path: item.path, name: item.name, isDirectory: item.isDirectory };
    }
  }
  return null;
}

// How a file's lightweight preview is rendered (SPEC §9): a monospace text/Markdown excerpt,
// a QuickLook thumbnail (image / PDF / document), or a generic kind icon.
export type PreviewKind = "text" | "thumbnail" | "generic";

// File extensions rendered as a dimmed monospace excerpt rather than a thumbnail (SPEC §9:
// "text or Markdown excerpt"). Extension-driven so the decision is instant and offline; an
// unlisted type falls through to a thumbnail attempt and then a generic icon.
const TEXT_EXTENSIONS = new Set([
  "txt", "md", "markdown", "rst", "log", "csv", "tsv",
  "json", "jsonc", "yaml", "yml", "toml", "ini", "conf", "cfg", "env", "properties",
  "xml", "svg", "html", "htm", "css", "scss", "less",
  "js", "jsx", "ts", "tsx", "mjs", "cjs",
  "rs", "go", "py", "rb", "php", "java", "kt", "kts", "swift", "c", "h", "cc",
  "cpp", "hpp", "cs", "m", "mm", "scala", "clj", "lua", "pl", "r", "dart", "zig",
  "sh", "bash", "zsh", "fish", "ps1", "bat",
  "sql", "graphql", "gql", "proto", "diff", "patch", "gradle", "make", "mk",
  "gitignore", "gitattributes", "dockerignore", "npmrc", "editorconfig", "dockerfile",
]);

// File extensions worth a QuickLook thumbnail (SPEC §9: image / first PDF page / document).
const THUMBNAIL_EXTENSIONS = new Set([
  "png", "jpg", "jpeg", "gif", "bmp", "tiff", "tif", "heic", "heif", "webp", "avif", "ico",
  "pdf",
  "pages", "key", "numbers", "doc", "docx", "ppt", "pptx", "xls", "xlsx", "rtf", "rtfd",
  "psd", "ai", "sketch", "eps",
  "mov", "mp4", "m4v", "avi", "mkv", "webm",
]);

// The extension of a file name, lowercased. A leading-dot name (`.gitignore`) uses its whole
// body as the extension so dotfiles classify by their well-known name.
function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  if (dot === -1) {
    return "";
  }
  if (dot === 0) {
    return name.slice(1).toLowerCase();
  }
  return name.slice(dot + 1).toLowerCase();
}

export function classifyPreview(name: string): PreviewKind {
  const ext = extensionOf(name);
  if (TEXT_EXTENSIONS.has(ext)) {
    return "text";
  }
  if (THUMBNAIL_EXTENSIONS.has(ext)) {
    return "thumbnail";
  }
  return "generic";
}

// Preview request budgets (SPEC §9, §10). The excerpt reads a few KB of the head; the
// thumbnail is requested at a size the 300px column can upsample from without wasting bytes.
export const PREVIEW_TEXT_MAX_BYTES = 4096;
export const PREVIEW_THUMBNAIL_MAX_PX = 512;

// The Preview Panel toggle (SPEC §9, §12: on/off, default on), read from Settings. When off
// the column is removed entirely — no animation (SPEC §9).
export function isPreviewPanelVisible(settings: Settings): boolean {
  return settings.previewPanelVisible;
}
