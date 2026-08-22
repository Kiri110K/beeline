# Specify Path Input failures

Type: grilling
Status: open
Blocked by: none

## Question

What should happen when the user submits a relative, mistyped, missing, deleted, inaccessible, or malformed path? Define which inputs Path Input resolves, whether a bare name such as `work` is searched for or rejected, how selection and the current Location survive failure, and how the UI explains the problem without exposing backend exception text such as `ENOENT` or covering the list with a persistent technical error.

## Comments

Kirill's Electron hands-on test submitted `wokr` and then `work`. The demo resolved both against its process working directory and displayed the raw `stat` exception across the bottom of the window. This is useful failure evidence, not production behavior.

Exact visual presentation may be refined during production development. The specification must still preserve three non-negotiable invariants before handoff: failure does not change the current Location, failure does not destroy the existing selection or scroll context, and backend exception text such as `ENOENT` is never exposed to the user.
