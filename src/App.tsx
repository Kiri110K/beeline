import type { ReactElement } from "react";

import { ActionMenu } from "./components/ActionMenu";
import { ConfirmBar } from "./components/ConfirmBar";
import { FileTable } from "./components/FileTable";
import { NavigationInput } from "./components/NavigationInput";
import { SearchResults } from "./components/SearchResults";
import { StatusStrip } from "./components/StatusStrip";
import { TabStrip } from "./components/TabStrip";
import { isPreviewPanelVisible, previewTargetFor } from "./preview/model";
import { PreviewPanel } from "./preview/PreviewPanel";
import { FirstRunBanner } from "./settings/FirstRunBanner";
import { SettingsView } from "./settings/SettingsView";
import { useSettingsStore } from "./settings/store";
import { useTabs } from "./tabs/useTabs";

// SPEC §3 layout, top to bottom: Tab strip, Navigation Input, dense file table,
// reserved Preview Panel column, Status Strip zone. In Search Mode the Search
// Results overlay floats over the file table without disturbing Browse state. The Settings
// view (§12) and the first-run guidance (§13) overlay the whole window when active.
export default function App(): ReactElement {
  const { settings, save } = useSettingsStore();
  const tabs = useTabs(settings, save);
  const browse = tabs.activeBrowse;
  const search = tabs.activeSearch;
  const rename = tabs.ops.rename;
  // The Preview Panel follows the Focused Item in Browse and Search Results (SPEC §9). The
  // column is removed entirely when Settings turns it off — no animation.
  const previewTarget = previewTargetFor(browse, search);
  return (
    <div className="flex h-screen flex-col bg-neutral-900 text-[13px] text-neutral-100">
      <TabStrip controller={tabs} />
      <NavigationInput controller={tabs} />
      <div className="flex min-h-0 flex-1">
        <div className="relative flex min-h-0 flex-1 flex-col">
          <FileTable
            load={browse.load}
            location={browse.location}
            focusedIndex={browse.focusedIndex}
            selected={browse.selected}
            pendingScrollTop={browse.pendingScrollTop}
            scrollGeneration={browse.scrollGeneration}
            renamePath={rename?.path ?? null}
            renameError={rename?.error ?? null}
            onSelect={tabs.select}
            onActivate={tabs.activateItem}
            onScrollTop={tabs.onScrollTop}
            onOpenMenu={tabs.openRowMenu}
            onCommitRename={tabs.commitRename}
            onCancelRename={tabs.cancelRename}
            onReachEnd={tabs.loadMoreRecents}
          />
          {search.mode === "search" ? (
            <SearchResults
              results={search.results}
              focusedIndex={search.focusedIndex}
              onReveal={tabs.revealResultAt}
            />
          ) : null}
        </div>
        {isPreviewPanelVisible(settings) ? (
          <PreviewPanel target={previewTarget} />
        ) : null}
      </div>
      <ConfirmBar controller={tabs} />
      <StatusStrip controller={tabs} />
      <ActionMenu controller={tabs} />
      {settings.firstRunDismissed ? null : (
        <FirstRunBanner settings={settings} save={save} />
      )}
      {tabs.settingsOpen ? (
        <SettingsView
          settings={settings}
          save={save}
          onClose={tabs.closeSettings}
        />
      ) : null}
    </div>
  );
}
