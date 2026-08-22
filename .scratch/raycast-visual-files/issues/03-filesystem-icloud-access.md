# Research filesystem and iCloud access

Type: research
Status: resolved
Blocked by: none

## Question

Which parts of the local filesystem, iCloud Drive, external volumes, network volumes, and hidden locations can a Raycast extension enumerate and operate on in practice? Document macOS and Raycast permission boundaries, on-demand iCloud placeholder behavior, download triggering, failure states, and any access that cannot be made reliable.

## Comments

## Answer

### Practical boundary

A Raycast extension can enumerate and modify any currently mounted path that the Raycast process can access as the logged-in user. It cannot guarantee access to the entire Mac.

Raycast runs extensions in a child Node.js process and does not add a separate file-I/O sandbox. macOS permissions apply to the parent Raycast app, so every installed extension shares Raycast's Files & Folders and Full Disk Access grants. See [Raycast's runtime and permission model](https://developers.raycast.com/information/security). Ordinary enumeration and mutation can use asynchronous Node file APIs. Raycast also has supported APIs for opening a path in the default or chosen application and moving an item to Trash. See [Node's file-system API](https://nodejs.org/api/fs.html) and [Raycast system utilities](https://developers.raycast.com/api-reference/utilities).

| Location | What works | Hard boundary or failure |
| --- | --- | --- |
| User-owned local files | List, stat, read, create, rename, copy, move, and trash when Unix permissions and macOS privacy allow it. | A path may disappear or change between listing and action. Operations must handle the actual error instead of trusting a prior access check. |
| Desktop, Documents, Downloads, iCloud Drive, app data | Works after the user grants the relevant permission to Raycast. | A denial applies to the Raycast process, not one extension. The extension cannot bypass it. Full Disk Access covers more protected data but is a large, app-wide permission. Apple documents these controls in [Controlling app access to files](https://support.apple.com/en-euro/guide/security/secddd1d86a6/web) and [Privacy & Security settings](https://support.apple.com/en-gb/guide/mac-help/mchl211c911f/mac). |
| Mounted removable and network volumes | They join the same filesystem namespace and can be browsed with normal file APIs. | They have separate privacy grants, filesystem permissions, credentials, read-only flags, and connection state. An unmounted or disconnected share is not browsable. Apple lists separate policies for [network and removable volumes](https://developer.apple.com/documentation/devicemanagement/privacypreferencespolicycontrol/services-data.dictionary) and describes the first-access prompt for [network volumes](https://developer.apple.com/documentation/bundleresources/information-property-list/nsnetworkvolumesusagedescription). |
| Dotfiles and hidden directories | They are ordinary directory entries to programmatic APIs. Node's `readdir` omits only `.` and `..`. | Hidden does not mean permitted. Other users' homes, protected app containers, ACL-restricted paths, and some administrative data remain inaccessible without the relevant rights. Symlinks can also lead outside the visible location or become broken. |
| System locations | Read access varies by path and permissions. | SIP and the sealed system volume prevent ordinary writes to Apple-managed locations even for unsandboxed apps. Apple documents the protected locations in [SIP file-system protections](https://developer.apple.com/library/archive/documentation/Security/Conceptual/System_Integrity_Protection_Guide/FileSystemProtections/FileSystemProtections.html). |

### iCloud Drive and other file providers

Cloud-backed entries are not always local files. Apple's File Provider model distinguishes a dataless item, which has metadata but no local contents, from a materialized item. A dataless directory may exist before the provider has enumerated its children. Opening that directory can therefore require network work. See [Synchronizing a File Provider extension](https://developer.apple.com/documentation/FileProvider/synchronizing-the-file-provider-extension).

For an iCloud item, Foundation exposes download state and errors, and `FileManager.startDownloadingUbiquitousItem` explicitly requests a download. A coordinated read can also trigger materialization, but Apple warns that it may block for a long time and fail if the item cannot be downloaded. See [`startDownloadingUbiquitousItem`](https://developer.apple.com/documentation/foundation/filemanager/startdownloadingubiquitousitem%28at%3A%29), [ubiquitous item status keys](https://developer.apple.com/documentation/foundation/urlresourcekey/ubiquitousitemisdownloadingkey), and [`NSFileCoordinator` coordinated reads](https://developer.apple.com/documentation/foundation/nsfilecoordinator/coordinate%28readingitemat%3Aoptions%3Aerror%3Abyaccessor%3A%29).

The public Raycast API exposes `open`, Quick Look, Trash, and application selection, but no documented API for cloud-placeholder status, download progress, or an explicit File Provider download request. Plain Node `fs` also does not expose Foundation's ubiquitous-item resource values. Therefore:

- Listing names and metadata can stay in the TypeScript extension, but it must represent a cloud entry as potentially unavailable until an operation succeeds.
- Opening a placeholder may let macOS or the target app materialize it, but the extension cannot promise immediate availability or useful progress reporting through Raycast's public API alone.
- A polished cloud state such as "not downloaded", "downloading", progress, and provider error needs a small native Foundation bridge or helper. The iCloud-specific download call is not a universal contract for third-party File Provider domains.
- Do not depend on the private on-disk shape of `~/Library/Mobile Documents` or undocumented tools such as `brctl`. They are not stable public integration points.

Offline mode, expired provider authentication, quota problems, unresolved cloud conflicts, a deleted remote item, and a provider that has not enumerated a folder can all turn a visible row into a failed open or mutation. These need first-class loading, retry, offline, permission-denied, read-only, and disappeared-item states. Apple also recommends coordinated asynchronous access because cloud file I/O can cause blocking network requests in [Improving performance and stability when accessing the file system](https://developer.apple.com/documentation/foundation/improving-performance-and-stability-when-accessing-the-file-system).

### Local verification on this Mac

A read-only inspection of the installed Raycast 1.104.25 app found no `com.apple.security.app-sandbox` entitlement, consistent with Raycast's documentation. Its `Info.plist` includes usage descriptions for Desktop, Documents, Downloads, removable volumes, and File Provider domains. This confirms that access prompts belong to Raycast itself. Recheck the installed v2 build before implementation because entitlements and usage descriptions can change between releases.

### Consequences for the specification

The browser should discover capability per operation and show the resulting state. It must not advertise a root as permanently readable or writable. Keep destructive actions routed through Trash where possible. Treat network access and cloud materialization as cancellable, timeout-prone work outside the instant folder-listing path. The two remaining technical blockers are explicit cloud status without a native helper and reliable access to locations for which the user has denied Raycast permission.
