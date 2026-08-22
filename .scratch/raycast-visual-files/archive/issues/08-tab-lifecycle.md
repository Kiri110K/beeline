# Decide Tab lifecycle

Type: grilling
Status: resolved
Assignee: /root
Blocked by: 17

## Question

What are the exact creation, closing, switching, pinning, unpinning, ordering, naming, overflow, navigation-history, restoration, and inactivity-reset rules for Temporary Tabs and Pinned Tabs? Decide whether entering a directory reuses the current Tab by default and which actions intentionally create another Tab.

## Comments

## Resolution

### Window and entry

- The product has exactly one application window. Tabs cannot become windows and additional windows cannot be created.
- The global shortcut behaves as a Raycast-style toggle: it shows and focuses a hidden window, focuses a visible window when another application is active, and hides Visual Files when Visual Files is already active.
- The window opens centered by default. It is freely draggable, and center guide lines provide a snap target for returning it to the centered position. Multi-monitor placement and per-display geometry are outside version-1 scope; a future default should use the screen of the frontmost application's key window and fall back to the screen containing the pointer.
- A new Temporary Tab starts at the Default Entry Point with the Navigation Input active. The Default Entry Point is Recents unless Settings names another Location.
- A warm return restores the active Tab, Location, focused and selected Items, scroll position, navigation history, and retained Navigation Input text. Quick Look, Action Menu, and active Search Results never survive hiding or restoration.

### Tab state and names

- Every Tab owns its current Location, back/forward history, Focused Item, Selected Items, current scroll position, and retained inactive Search Query.
- A Temporary Tab's title follows its current Location. A Search Query never becomes the Tab title.
- A Pinned Tab's title is the Anchor name by default and does not change during a Pinned Excursion. The title can be renamed without changing the Anchor.

### Pinned Tabs

- A Pinned Tab is a protected, persistent Anchor rather than a permanently mutable browser session.
- Normal directory navigation from a Pinned Tab uses the same Tab and creates or continues a Pinned Excursion. Switching between Tabs preserves that Excursion; it does not reset on screen.
- Clicking the already-active Pinned Tab returns it to its Anchor.
- After the application has spent five continuous minutes in the background, every Pinned Excursion is discarded on the next application invocation and the Tabs return to their Anchors. Nothing resets while the window is visible. Time spent while the Mac sleeps counts toward the five minutes, but reset is applied only on the next invocation.
- Pinning a Temporary Tab makes its current Location the Anchor, assigns the Location's name, and moves the Tab into the pinned group. A collection such as Recents can also be an Anchor.
- Two Pinned Tabs cannot have the same Anchor. A request to pin a duplicate activates the existing Pinned Tab.
- Unpinning converts the Tab to Temporary without changing its current Location and starts its Temporary lifetime at that moment.
- Pinned Tabs survive application quit, restart, and crashes through promptly persisted Anchor, custom name, and order. Pinned Excursions, selection, scroll, and history do not survive a full process restart.

### Search routing and reuse

- Search routing chooses the most specific matching Anchor when multiple Pinned Tabs contain a result.
- Revealing a file below an Anchor activates that Pinned Tab, navigates to the file's containing Location, and makes the file the Focused Item. This is a Pinned Excursion and does not open the file's external application.
- Revealing an Anchor itself activates the Pinned Tab at the Anchor.
- Revealing a directory below an Anchor opens or reuses a Temporary Tab at that directory instead of moving the Pinned Tab.
- A result outside all Anchors opens or reuses a Temporary Tab when Search began in a Pinned Tab. Search begun in a Temporary Tab reuses that Tab.
- Automatic Search and navigation routing reuses an existing Temporary Tab already at the target Location. Explicit `Open in New Tab` always creates a new Temporary Tab and may intentionally duplicate a Location.
- Ordinary directory entry otherwise reuses the current Tab.

### Temporary Tabs

- Temporary Tabs are disposable. Their default lifetime is three hours since last activation.
- Settings offers `30 minutes`, `1 hour`, `3 hours`, `6 hours`, `12 hours`, `24 hours`, and `Never`; `3 hours` is the default.
- Expired Tabs are removed only while the application is already in the background or during the next invocation. A Tab never disappears while the user is looking at the window. The active Tab at the moment of hiding can expire normally.
- Temporary Tabs do not survive a full application restart.
- After five minutes in the background but before the active Temporary Tab expires, the same Temporary Tab is restored. The five-minute Pinned reset and the Temporary lifetime are independent rules.
- If no Tab remains, the application immediately creates a clean Temporary Tab at the Default Entry Point; an empty tabless window does not exist.

### Ordering, switching, and overflow

- Pinned Tabs form a user-ordered group on the left; Temporary Tabs form a user-ordered group on the right.
- Dragging within a group reorders Tabs. Dragging a Temporary Tab into the pinned group pins it at the drop position; dragging a Pinned Tab into the temporary group unpins it. The cross-group effect is immediate and reversible by dragging back.
- A new Temporary Tab is inserted beside the originating Tab while remaining after the entire pinned group. Closing it returns activation to the Tab that created it; if that Tab no longer exists, activation goes to the nearest Tab on the left.
- `Command+1` through `Command+8` activate Tabs by visible left-to-right position. `Command+9` activates the last Tab. The mapping follows user reordering.
- `Control+Tab` and `Control+Shift+Tab` cycle left-to-right and right-to-left. Most-recently-used cycling is explicitly deferred.
- Tabs first shrink to a readable minimum of approximately 120–140 points. Beyond that, the strip scrolls horizontally and exposes an all-Tabs control with a searchable name list. Exact dimensions and overflow feel remain subject to production UX testing.

### Closing and menus

- `Command+W` closes an active Temporary Tab.
- `Command+W` on a Pinned Tab hides the single application window instead of removing the Anchor.
- A Pinned Tab has no incidental close control. Removing it requires the explicit `Remove Pinned Tab` action.
- The version-1 Temporary Tab menu contains `Pin`, `Close`, and `Copy Location`.
- The version-1 Pinned Tab menu contains `Rename`, `Unpin`, `Remove Pinned Tab`, and `Copy Location`.
- `Duplicate`, `Close Others`, and bulk pinning are outside version 1. Detached Tabs and multi-window commands are outside the product model.

## Acceptance scenarios

1. Navigate several directories away from a Pinned Tab's Anchor, switch to another Tab and back, and observe the same Pinned Excursion. Click the active Pinned Tab and observe an immediate return to the Anchor.
2. Hide the application for less than five minutes and recover the exact warm state. Hide it for more than five minutes and observe all Pinned Tabs at their Anchors without any reset occurring while visible.
3. Reveal a file within the deepest matching Anchor and observe that Pinned Tab navigate to the containing Location with the file focused. Reveal a directory in the same tree and observe a Temporary Tab instead.
4. Open a Location already present in a Temporary Tab through Search and observe reuse. Invoke explicit `Open in New Tab` and observe a deliberate duplicate.
5. Leave a Temporary Tab inactive past the configured lifetime. Confirm that it remains while the application is visible and is removed only in the background or on the next invocation.
6. Quit and relaunch. Confirm that Pinned Anchor, custom name, and order return, Temporary Tabs do not, and a fresh Default Entry Point is active.
7. Attempt to close a Pinned Tab with `Command+W` and observe the window hide without losing the Anchor. Remove it through the explicit menu action and observe persistent removal.
8. Drag a Tab across the group boundary and observe Pin or Unpin, then drag it back to reverse the change.
9. Use `Command+1…9` and `Control+Tab` after reordering and overflow; activation follows the visible ordering.
