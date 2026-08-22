# Decide the navigation and action model

Type: grilling
Status: resolved
Assignee: /root
Blocked by: 17

## Question

What exact keyboard and mouse contract should govern selection, enter-directory, open-file, back/forward, Quick Look, action discovery, open-in-new-Tab, path submission, and cancellation while keeping the global shortcut count near one? Resolve the roles of arrow keys, Enter, Space, `Ctrl+J/K`, double click, context click, and the action menu.

## Comments

## Answer

### Navigation states

The application has two logical states independent of DOM focus.

- A new Temporary Tab starts in Search Mode with an active Navigation Input and its Default Entry Point underneath it. The Default Entry Point is Recents unless Settings selects another Location.
- Search Mode shows a separate Search Results layer below the Navigation Input. The first result is focused automatically. Up and Down move through results; Left and Right edit the query; Enter, Right Arrow, or a single click performs Reveal.
- Revealing a file opens its containing Location in Browse Mode and makes the file the Focused Item. Revealing a directory enters it as the Location. Reveal never opens a file in an external application.
- Browse Mode routes Up and Down through files and directories, Left to navigation history, and Right or Enter to the Focused Item's primary action. `Command+[` and `Command+]` always move back and forward. History entries restore selection and scroll position.
- `Control+J` and `Control+K` mirror Down and Up in Browse Mode, Search Results, and Quick Look. They are not intercepted while the Navigation Input is active, so macOS text editing keeps its normal behavior. `Command+K` remains distinct and opens the Action Menu.
- Escape from Search Mode hides Search Results and restores the previous Browse selection and scroll position. The query remains visible but inactive and is not selected. Clicking the Navigation Input, pressing `Command+L`, or pressing the physical Slash key reactivates Search Mode and selects the retained query. The physical key binding must work on both English and Russian layouts.
- Plain letters do nothing in Browse Mode in version 1. Query history and type-to-filter within the current Location are outside version 1. `h/j/k/l` remain available for a later Vim Navigation option.

Search Results are a floating list over the Browse list, not a replacement for the current Location. They combine exact paths, frequently and recently visited Locations, current-Location matches, and global Spotlight files and directories. RU and EN keyboard-layout correction is required; phonetic transliteration is not. Results arrive progressively. Reordering is allowed until the user starts keyboard navigation, after which the focused row must remain stable.

### Selection and mouse

- One Focused Item receives navigation, Reveal, Preview Panel, Quick Look, and primary actions.
- Selected Items receive batch actions. Up and Down collapse selection to the focused row. `Shift+Up/Down` and `Shift+Click` extend a range. `Command+Click` toggles individual Items.
- Enter and Right Arrow act only on the Focused Item even when several Items are selected. Batch Open is an explicit Action Menu command.
- In Browse Mode, one click selects, double click performs the primary action, and context click opens the Action Menu.
- In Search Results, one click performs Reveal. Direct actions on a result remain available through the Action Menu.
- Version 1 exposes Open in New Tab through the Action Menu. `Command+Click`, context-click placement, and middle-click behavior are deferred to the mockup pass.

### Primary actions and actions menu

Directory and file behavior are configured independently. The defaults are Enter Location for a directory and Open with Default App for a file. Either kind may instead use Show Action Menu as its primary action.

`Command+K`, the `…` control, and context click open the same Action Menu. With Selected Items it shows applicable file actions. With no selection it shows application actions such as New Tab, Paste Path, Refresh, Settings, and Quit. Quick Look and Action Menu never coexist: opening either closes the other first.

The initial deletion shortcut is `Command+Delete`; plain Delete has no file action in Browse Mode and edits text normally when the Navigation Input is active. The deletion shortcut must be configurable later.

### Preview Panel and Quick Look

Preview Panel and Quick Look are separate features.

- Preview Panel is a fixed-width right column, enabled by default and disabled through Settings. It has no show or hide animation. It follows the Focused Item in Browse and Search Results without changing navigation behavior.
- For files, Preview Panel progressively shows immediate metadata, images, a text or Markdown excerpt, the first PDF page, or a generic icon and metadata. Old preview work must never block navigation or overwrite a newer selection. For directories it shows path and metadata without recursively calculating size.
- Space opens full-document Quick Look for the Focused Item. While Quick Look is active, Up and Down move only through files. With multi-select they move through selected files; otherwise they move through all files in the presented list. Closing Quick Look preserves the last focused file and the original multi-selection when one existed.

### Escape and post-action behavior

Escape closes the active layer in this order: Quick Look, Action Menu, Search Results, Selected Items, then the application window. Closing Search Results preserves the prior Browse selection. Preview Panel is persistent and is not affected by Escape.

Settings contain an After Action table with `Hide Window` or `Keep Open` per action. Defaults are Hide for Open File, Open in Terminal or IDE, Copy Path, and Copy File; Keep Open for Trash, Quick Look, Enter Directory, and navigation. Shift has no temporary inversion behavior in version 1. Copy File puts a file reference on the clipboard, Copy Path puts textual paths on the clipboard, and Duplicate is a separate operation with no special version-1 role.
