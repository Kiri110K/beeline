# Decide Recents and filesystem entry points

Type: grilling
Status: resolved
Assignee: /root
Blocked by: 02, 03, 17

## Question

How should Recents, Home, Downloads, work folders, iCloud Drive, mounted volumes, hidden locations, and arbitrary pasted paths appear without recreating Finder's sidebar complexity? Specify the default collection, sorting and filtering controls, empty/error states, and the relationship between Pinned Tabs and reusable filesystem entry points.

## Comments

Kirill selected Recents as the default Default Entry Point. Settings must allow replacing it with another Location. Every automatically created clean Temporary Tab uses this configured Default Entry Point.

## Resolution

### Entry model

- There is no sidebar and no persistent places panel. Search is the primary function of an opened window; standard Locations are reached by typing, not by pointing at a list.
- Known Places — Home, Desktop, Documents, Downloads, iCloud Drive, and mounted volumes — are a ranking boost inside Search Results, not a separate navigation mechanism. They are a small fraction of what Search must find, so they cannot be the search model.
- The Alias Dictionary in Settings maps typed words to Locations, for example «загрузки» to `~/Downloads`. An alias is a ranking recommendation, never an exclusive match. It is distinct from the RU/EN layout correction of the navigation model, which fixes wrong-layout typing but does not translate words.
- Work folders and other personal entry points remain Pinned Tabs; the ticket 08 contract is unchanged.
- Arbitrary pasted paths remain Navigation Input work; error behavior is ticket 18.
- A mounted external or network volume is a Known Place while mounted and disappears on unmount. Mount management, ejecting, and connecting shares are outside version 1; ejecting may later join the operations ticket.

### Visit Journal

- From version 1 the application keeps a Visit Journal: Locations entered and files opened through the application. It exists solely as ranking input for Search and is not a user-facing collection.
- This revises the map decision that excluded an extension-owned visit-history index from version 1. That decision was recorded before search became the primary entry mechanism. The Recents collection itself stays system-derived.
- What exactly the journal records and how the ranker weighs it is designed in the ranking-search ticket.

### Hidden locations

- Directory listings always show hidden entries; no toggle is required to see them. Their exact sort placement is a production UX detail.
- Search penalizes hidden Items slightly instead of excluding them. A direct name match outweighs the hidden penalty.
- How hidden locations become searchable at all is undecided and belongs to the ranking-search ticket. A whitelist grown by explicit visits is the first candidate, not a decision. Junk-heavy directories such as agent session stores must not be indexed wholesale.
- Measured 2026-08-22 on this Mac: Spotlight holds index records for files under dot-directories but returns them from neither global nor scoped name queries, so hidden-location search cannot ride on Spotlight.

### Recents collection

- The default Recents Tab shows the system-derived collection per research 02: files only, no directories, newest last-used first.
- Version 1 has no sorting or filtering controls on the Recents view; a different order may later become a Settings option. Typing in the Navigation Input starts global Search, not a filter of the visible list.
- Loading is progressive: the first visible rows render immediately from the cached result, and further batches of 100 load in the background as the user scrolls. There is no hard product cap; depth is bounded by scrolling.
- Items the application cannot access stay visible and fail with an explanatory error on action. The application expects near-complete disk access; silently dropping rows would make Recents lie.
- Two degraded states exist: an honestly empty collection, and Spotlight unavailable — disabled, excluded by privacy settings, or still indexing — with an explanation. There is no silent fallback to a filesystem crawl. Exact copy and diagnostics may be tuned during whole-application testing.
- Hidden and non-user-facing support files are rejected by the documented post-filter from research 02.

## Acceptance scenarios

1. Open a new window and type a Known Place name in either layout, or an alias such as «загрузки»; the place ranks at the top without any sidebar existing.
2. Type a query that matches a hidden Item by exact name and visible Items fuzzily; the exact-name hidden Item outranks them despite the hidden penalty.
3. Browse the home directory and see dot-directories listed without toggling anything.
4. Open the Recents Tab; visible rows appear instantly and scrolling loads further batches of 100 without blocking the list.
5. Revoke access to a folder; its file stays visible in Recents and acting on it produces an explanatory error.
6. Disable Spotlight indexing and observe the explicit unavailable state instead of an empty list or a disk crawl.
7. Enter Locations and open files, then observe later Search Results rank them higher, per the ranking-search ticket's mechanics.
