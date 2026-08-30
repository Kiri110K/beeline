# Beeline

A personal file-browsing context for reaching and operating on files without falling back to Finder. Beeline is the version-1 working name (formerly Visual Files); the final name is decided before any public release.

## Language

**Tab**:
An independent browsing session with its own current Location and navigation history. A Tab is either temporary or pinned.
_Avoid_: Window, view, Place

**Temporary Tab**:
A disposable Tab for transient browsing. A newly created Temporary Tab starts at the Default Entry Point with the Navigation Input ready.
_Avoid_: Ephemeral window, search tab

**Pinned Tab**:
A persistent, named Tab with an Anchor and an optional short-lived Pinned Excursion.
_Avoid_: Favorite, bookmark, Place

**Anchor**:
The persistent starting Location or collection of a Pinned Tab.
_Avoid_: Home folder, root, favorite

**Pinned Excursion**:
The transient browsing state created when navigation moves a Pinned Tab away from its Anchor.
_Avoid_: Temporary Tab, changed Anchor

**Default Entry Point**:
The collection or Location used by a newly created Temporary Tab. It is Recents unless the user chooses another Location.
_Avoid_: Home page, start tab

**Location**:
The directory currently being browsed by a Tab.
_Avoid_: Tab, Place, workspace

**Item**:
A file-system entry presented for inspection or action, either a file or a directory.
_Avoid_: Result, document

**Recents**:
A system-derived collection of recently used Items. Recents is a collection, not a directory or navigation history.
_Avoid_: History, recent folder

**Name Index**:
The application-owned index of Item names and paths that answers Search Queries. It covers hidden and Junk content as well as visible content.
_Avoid_: Spotlight, database, cache

**Junk**:
Pattern-classified content, such as dependency, build, cache, and agent-session directories, that stays searchable but carries a heavy ranking penalty and refreshes lazily.
_Avoid_: Excluded, ignored, hidden

**Visit Journal**:
The application-owned record of Locations entered and files opened through the application, used solely as ranking input for Search Results.
_Avoid_: History, Recents, log

**Known Place**:
A standard Location the application recognizes ahead of time, such as Home, Desktop, Documents, Downloads, iCloud Drive, or a mounted volume. Known Places receive a ranking boost in Search Results.
_Avoid_: Sidebar item, favorite, bookmark

**Alias Dictionary**:
The user-editable mapping from typed words to Locations that recommends a target to Search ranking without excluding other results.
_Avoid_: Bookmarks, shortcuts, translation

**Navigation Input**:
The text entry point that interprets typed or pasted text as a path or Search Query.
_Avoid_: Path Input, search box, address bar

**Search Query**:
Text in the Navigation Input used to find Items and Locations when it does not resolve directly to a path. Its identity ignores case, surrounding and repeated whitespace, and canonically equivalent Unicode encoding, but preserves punctuation.
_Avoid_: Path, filter

**Path Interpretation**:
An ordered reading of a Search Query in which separate query tokens identify distinct path components, with any number of intermediate components omitted. It competes with ordinary search, needs no path separators, and may end at either a file or a directory.
_Avoid_: Implicit Path, Location Chain, path mode

**Query Family**:
Ordinary interpretations of normalized Search Queries with the same distinct tokens regardless of token order or repetition. Members share ordinary-search evidence in Search Memory at full strength; Path Interpretation evidence remains ordered.
_Avoid_: Query history, permutation

**Working Set**:
The personal set considered for every Search Query regardless of length: Items in the current Location, Pinned Anchors, system Recents, paths from the Visit Journal, and Items with retained query-independent usage or Search Memory. It has no total Item limit: the current Location is included in full, historical sources are bounded independently, and unvisited descendants of a Pinned Anchor are not included merely because their ancestor is pinned.
_Avoid_: Local index, Recents, cache

**Search Memory**:
The local, persistent association between an Item, the normalized Search Query, and any currently supported ordinary or Path Interpretation when the user opens its Action Menu, invokes Quick Look, or completes an Item action. It may return or promote the Item for the same or a sufficiently similar future query, and follows the Item across rename and move; visibility, scrolling, hover, focus, selection, and choosing another Item are not signals.
_Avoid_: Visit Journal, Recents, history, cache

**Search Results**:
A transient ranked list of Items and Locations matching a Search Query. Search Results do not change the current Location until the user Reveals one.
_Avoid_: Folder contents, Recents

**Reveal**:
The transition from a Search Result to Browse Mode without performing the Item's configured primary action. Revealing a file opens its containing Location and focuses it; revealing a directory enters that Location.
_Avoid_: Open, preview, select

**Browse Mode**:
The state with no active Search Results overlay in which navigation acts on the current Location or collection. The Navigation Input may retain the previous Search Query.
_Avoid_: List focus, inactive search

**Search Mode**:
The state with a Search Query in which navigation acts on the Search Results overlay.
_Avoid_: Input focus, filter mode

**Focused Item**:
The single Item that receives navigation, preview, and primary actions in the currently presented list.
_Avoid_: Active row, cursor

**Selected Items**:
The set of Items targeted together by a file action. The Focused Item anchors range selection and belongs to the set when it is selected.
_Avoid_: Focus, checked Items

**Action Menu**:
The context-sensitive list of available Item actions or application actions.
_Avoid_: Command palette, context menu

**Preview Panel**:
The persistent in-application panel that shows an immediate lightweight preview and metadata for the Focused Item.
_Avoid_: Quick Look, Open, viewer

**Quick Look**:
The temporary full-document preview opened for a file without handing it to an editing application.
_Avoid_: Preview Panel, Open, viewer

**Status Strip**:
The persistent zone of the window that shows progress and failures of asynchronous work, from batch operations to cloud downloads. It is the application's only feedback surface.
_Avoid_: Toast, notification, alert

**Accessible Filesystem**:
The local, cloud-backed, or mounted file-system content that macOS permissions make available to the application.
_Avoid_: Disk, Finder files
