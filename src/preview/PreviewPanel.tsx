import { type ReactElement, type ReactNode } from "react";
import { match } from "ts-pattern";

import { FileGlyph, FolderGlyph } from "../components/icons";
import { formatModified, formatSize } from "../location/format";
import { strings } from "../strings";
import type { PreviewMetadata } from "./ipc";
import type { PreviewTarget } from "./model";
import { usePreview, type PreviewMetaView, type PreviewView } from "./usePreview";

// One labelled metadata row (Kind / Size / Created / Modified / Items). Rendered only when
// the value is present, so a file never shows an empty "Items" line nor a directory a "Size".
function MetaRow({
  label,
  value,
}: {
  label: string;
  value: string;
}): ReactElement | null {
  if (value === "") {
    return null;
  }
  return (
    <div className="flex gap-2">
      <span className="w-16 shrink-0 text-neutral-500">{label}</span>
      <span className="min-w-0 flex-1 truncate text-neutral-300">{value}</span>
    </div>
  );
}

// The header metadata block: size / kind / timestamps for a file, child count for a directory
// (no recursive size, SPEC §9). Nothing until `preview_metadata` lands; an honest line when
// the Item vanished.
function MetaBlock({ meta }: { meta: PreviewMetaView }): ReactElement | null {
  return match(meta)
    .with({ status: "pending" }, () => null)
    .with({ status: "missing" }, () => (
      <div className="text-neutral-500">{strings.preview.unavailable}</div>
    ))
    .with({ status: "ready" }, ({ metadata }) => <MetaRows metadata={metadata} />)
    .exhaustive();
}

function MetaRows({ metadata }: { metadata: PreviewMetadata }): ReactElement {
  return (
    <div className="flex flex-col gap-1">
      <MetaRow label={strings.preview.kind} value={metadata.kind} />
      {metadata.isDirectory ? (
        <MetaRow
          label={strings.preview.itemsLabel}
          value={
            metadata.childCount === null
              ? ""
              : strings.preview.items(metadata.childCount)
          }
        />
      ) : (
        <MetaRow
          label={strings.preview.size}
          value={formatSize(metadata.sizeBytes)}
        />
      )}
      <MetaRow
        label={strings.preview.created}
        value={formatModified(metadata.createdMs)}
      />
      <MetaRow
        label={strings.preview.modified}
        value={formatModified(metadata.modifiedMs)}
      />
    </div>
  );
}

// The lightweight body: a thumbnail (asset protocol), a dimmed monospace excerpt, or a large
// kind glyph. The thumbnail arrival re-renders only this panel, never the file table (§10).
function PreviewBody({
  view,
  target,
}: {
  view: PreviewView;
  target: PreviewTarget;
}): ReactElement {
  const body: ReactNode = match(view.body)
    .with({ status: "pending" }, () => <KindGlyph target={target} muted />)
    .with({ status: "none" }, () => <KindGlyph target={target} />)
    .with({ status: "image" }, ({ src }) => (
      <img
        src={src}
        alt=""
        className="max-h-64 max-w-full rounded object-contain"
      />
    ))
    .with({ status: "text" }, ({ text, truncated }) => (
      <pre className="max-h-64 w-full overflow-hidden whitespace-pre-wrap break-words font-mono text-xs leading-snug text-neutral-400">
        {text}
        {truncated ? strings.preview.truncationMark : ""}
      </pre>
    ))
    .exhaustive();
  return (
    <div className="flex min-h-[6rem] items-center justify-center py-2">
      {body}
    </div>
  );
}

// The generic kind icon, sized up for the body area. `muted` dims it while a thumbnail or
// excerpt is still in flight so the pending state reads as tentative.
function KindGlyph({
  target,
  muted = false,
}: {
  target: PreviewTarget;
  muted?: boolean;
}): ReactElement {
  return (
    <div
      className={`[&_svg]:h-10 [&_svg]:w-10 ${muted ? "text-neutral-700" : "text-neutral-600"}`}
    >
      {target.isDirectory ? <FolderGlyph /> : <FileGlyph />}
    </div>
  );
}

// The persistent fixed-width right column (SPEC §9). It follows the Focused Item passed in
// `target`; when the column is toggled off in Settings it is not rendered at all (App owns
// that decision), so this component always renders the column when mounted.
export function PreviewPanel({
  target,
}: {
  target: PreviewTarget | null;
}): ReactElement {
  const view = usePreview(target);
  return (
    <div
      className="flex w-[300px] shrink-0 flex-col gap-3 overflow-auto border-l border-neutral-800 p-3 text-[13px]"
      aria-label={strings.zones.previewPanel}
    >
      {view === null ? (
        <div className="flex flex-1 items-center justify-center text-neutral-600">
          {strings.preview.empty}
        </div>
      ) : (
        <>
          <PreviewBody view={view} target={view.target} />
          <div className="truncate font-medium text-neutral-100">
            {view.target.name}
          </div>
          <MetaBlock meta={view.meta} />
        </>
      )}
    </div>
  );
}
