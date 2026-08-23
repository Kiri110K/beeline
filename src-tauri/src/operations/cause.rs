//! Per-item failure causes (SPEC §8, §13): concrete human sentences, never a raw
//! `io::Error` Debug dump. One function maps an `io::ErrorKind` plus the action verb
//! and subject path into the sentence that reaches the Status Strip.

use std::io;
use std::path::Path;

/// Turn a failed operation into a short, concrete Status Strip sentence. `action` is a
/// present-participle verb ("copying", "moving", "trashing", "deleting", "renaming",
/// "creating folder") so the sentence reads "Permission denied while copying
/// /Users/k/x".
///
/// Only the three universally-stable `ErrorKind`s are named individually (newer kinds
/// like `NotADirectory` need a recent rustc); anything else still yields a bounded
/// sentence with the OS message as a parenthetical — never the whole `{error:?}` struct.
pub fn describe(action: &str, path: &Path, error: &io::Error) -> String {
    let subject = path.display();
    match error.kind() {
        io::ErrorKind::NotFound => format!("No longer exists while {action}: {subject}"),
        io::ErrorKind::PermissionDenied => {
            format!("Permission denied while {action}: {subject}")
        }
        io::ErrorKind::AlreadyExists => format!("Already exists while {action}: {subject}"),
        _ => format!("Could not finish {action} {subject} ({error})"),
    }
}
