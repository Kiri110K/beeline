# Research system Recents

Type: research
Status: resolved
Blocked by: none

## Question

What reliable and acceptably fast mechanism can a Raycast extension use to present macOS system Recents without opening Finder? Specify the observable semantics, relevant metadata APIs or commands, privacy/Spotlight dependencies, sorting and filtering behavior, and known cases where the result differs from Finder Recents.

## Comments

## Answer

### Recommendation

Use Spotlight metadata as the source of truth. For version 1, run `/usr/bin/mdfind` from the extension's Node process, fetch `kMDItemLastUsedDate` with batched `/usr/bin/mdls` calls, sort in the extension, and cache the last successful result. Do not automate Finder and do not depend on Raycast's private file index. Raycast documents that extensions run in a child Node process and can reach only the Raycast operations exposed by its public RPC API. There is no public File Search result API. [Raycast extension runtime](https://developers.raycast.com/information/security) and [Raycast File Search](https://manual.raycast.com/file-search).

If the shell adapter misses the final performance budget, keep the same query semantics and replace only the adapter with a small native helper around `NSMetadataQuery` or `MDQuery`. Apple supports asynchronous initial gathering, live updates, search scopes, returned metadata attributes, and sort descriptors through `NSMetadataQuery`. [Apple's metadata query guide](https://developer.apple.com/library/archive/documentation/Carbon/Conceptual/SpotlightQuery/Concepts/QueryingMetadata.html) and [current `NSMetadataQuery` reference](https://developer.apple.com/documentation/foundation/nsmetadataquery).

### Observable Finder semantics

On this machine, macOS 26.5.1, Finder ships its Recents search at `/System/Library/CoreServices/Finder.app/Contents/Resources/MyLibraries/myDocuments.cannedSearch/Resources/search.savedSearch`. Its raw query is:

```text
(kMDItemLastUsedDate = "*") &&
((kMDItemContentTypeTree = public.content) ||
 (kMDItemContentTypeTree = "com.microsoft.*"cdw) ||
 (kMDItemContentTypeTree = public.archive))
```

The saved search also sets `FinderFilesOnly`, `UserFilesOnly`, and the private scope `kMDQueryScopeMyFiles`. It has no age cutoff. This is local evidence from Apple's signed Finder bundle, not a public compatibility contract, so the extension must not read that bundle at runtime.

Apple defines `kMDItemLastUsedDate` as the time LaunchServices last opened the file, including a double click or a LaunchServices open request. It is not the modification date or POSIX access time. A command-line read, background scan, or application behavior that bypasses LaunchServices may not update it. [Apple's `kMDItemLastUsedDate` reference](https://developer.apple.com/documentation/coreservices/kmditemlastuseddate). Apple also documents Finder Recents and the Apple menu's Recent Items as separate mechanisms. The latter is therefore the wrong source. [Apple's Mac User Guide](https://support.apple.com/guide/mac-help/if-you-cant-find-a-file-on-mac-mchlp2305/mac).

The Finder predicate explains the useful baseline:

- Include files with a non-null last-used date whose type is content, a Microsoft content type, or an archive.
- Exclude directories. Finder Recents is a file collection, not folder visit history.
- Sort by `kMDItemLastUsedDate` descending. Use localized display name and then full path as stable tie breakers.
- Keep only the first product-defined limit after sorting. A limit of 100 is a sensible starting point for the later UX decision.

### Fast query plan

`mdfind` has no documented sorting or attribute-output option. Use NUL-delimited paths, then call `mdls -raw -name kMDItemLastUsedDate` in batches and zip the returned dates to those paths. Filter entries that disappeared during the query, deduplicate identical paths, and apply the final sort in memory.

Avoid paying for all historical matches on every opening. Query a recent window first with the documented `$time.today(-N)` syntax, sort it, and widen only when it produces fewer than the required result limit. A practical sequence is 90 days, 365 days, then no cutoff. Widening preserves the true newest-first top set because every newly admitted item is older than the previous window. [Apple's Spotlight query expression syntax](https://developer.apple.com/library/archive/documentation/Carbon/Conceptual/SpotlightQuery/Concepts/QueryFormat.html).

Read-only measurements on this Mac support that plan. The Finder-like predicate matched 1,265 items. `mdfind` returned all paths in 0.18 seconds, while fetching every last-used date raised the total to 0.89 seconds. The 90-day query matched 113 items and returned paths plus dates in 0.20 seconds. These are terminal measurements, not a Raycast benchmark. The performance prototype still needs to measure cold and warm behavior inside Raycast. First paint should use the cached result and refresh it asynchronously.

### Privacy and completeness

This source is reliable only within Spotlight's indexed and permitted view of the filesystem.

- Spotlight Privacy exclusions remove locations from the result. Reindexing can also take time, so a query reads an eventually updated metadata store rather than walking the disk. [Apple's Spotlight indexing guidance](https://support.apple.com/en-gb/102321).
- Non-indexed disks, some network volumes, optical media, disk images, and system locations may lack `kMDItemLastUsedDate`. Apple says rich metadata is not available on every volume and lets users exclude paths and document types from search. [Apple's metadata query guide](https://developer.apple.com/library/archive/documentation/Carbon/Conceptual/SpotlightQuery/Concepts/QueryingMetadata.html#//apple_ref/doc/uid/TP40001841-CJBEJBHH).
- The child process inherits Raycast's macOS privacy grants. Reading, previewing, deleting, or opening a returned path may still fail if Raycast lacks access to Desktop, Documents, Downloads, iCloud containers, removable volumes, or protected data. Raycast itself recommends Full Disk Access or grants for individual folders for file-backed features. [Raycast file permissions](https://manual.raycast.com/screenshots#permissions).
- An iCloud metadata record does not guarantee that file bytes are local. Recents can list the item promptly, while preview or open may need a download and can fail offline.

The UI must distinguish a healthy empty result from disabled indexing, a privacy exclusion, a still-building index, and a stale path. Silently falling back to a filesystem crawl would change the meaning of Recents and make startup unpredictable.

### Known differences from Finder

Exact parity is not available through documented public behavior. Finder's `kMDQueryScopeMyFiles`, `FinderFilesOnly`, and `UserFilesOnly` controls are private. A public query over all indexed locations can include items Finder suppresses, while restricting it to the home directory would omit external-volume items that Finder may show. Version 1 should use the public content predicate, reject hidden and non-user-facing support files in a documented post-filter, and treat small mismatches as expected.

Finder also has broader system privileges than a Raycast extension. Its list may contain an item that the extension can discover through metadata but cannot operate on. The product specification must define whether such an item stays visible with a permission error or is omitted. That is a UX decision for `Define Recents and filesystem entry points`, not part of this research answer.

### Resolution

The version 1 mechanism is a cached, asynchronous Spotlight query implemented first with `mdfind` plus batched `mdls`, using Finder's public metadata predicate and progressive time windows. It is fast enough to prototype on the current machine, keeps Finder closed, and has a clear native-helper upgrade path. The unavoidable blockers are Spotlight health, Raycast's macOS permissions, cloud or removable-volume availability, and the impossibility of reproducing Finder's private scope and filters exactly.
