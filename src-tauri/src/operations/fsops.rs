//! Pure filesystem primitives behind the operations engine (SPEC §8). No Tauri, no
//! threads, no telemetry — just the collision suffix, recursive copy, move-with-EXDEV,
//! and tree removal, so every rule here is unit-testable against a tempdir.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// EXDEV ("cross-device link") errno on both macOS and Linux. `fs::rename` cannot span
/// filesystems, so a move that hits this falls back to copy + delete (SPEC §8).
const EXDEV: i32 = 18;

/// Does anything at all live at `path`, including a broken symlink? `Path::exists`
/// follows symlinks and reports `false` for a dangling link, which would let us pick a
/// name that is actually taken; `symlink_metadata` answers about the link itself.
fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Split a basename into its stem and dot-extension for suffixing. A leading dot is part
/// of the stem (a dotfile has no bumpable extension), and only the last dot splits, so
/// `archive.tar.gz` bumps as `archive.tar 2.gz` — matching Finder's feel.
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        // `i > 0` keeps a leading dot with the stem (".gitignore" stays whole).
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Resolve a collision-free path for `name` inside `dir` with a numeric suffix
/// ("name 2.ext", "name 3.ext", …), the one place the suffix format is decided (SPEC §8,
/// §15). Returns the bare join when nothing collides.
///
/// This is best-effort against a racing writer: the chosen name can be taken between the
/// check here and the caller's create/copy, which then fails with `AlreadyExists` and is
/// reported as that item's concrete cause. Acceptable for a single-user browser.
pub fn resolve_collision(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !path_exists(&first) {
        return first;
    }
    let (stem, ext) = split_name(name);
    let mut n: u64 = 2;
    loop {
        let candidate = dir.join(format!("{stem} {n}{ext}"));
        if !path_exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Recursively copy `src` to `dst` (files and directories), preserving permissions where
/// std allows and never following a symlink into recursion — the link itself is copied
/// (SPEC §8, point 4). `dst` must not already exist.
///
/// A failure mid-tree leaves the partial copy in place; v1 has no undo (SPEC §8), so the
/// caller reports the cause and the user resolves it — no rollback is attempted here.
pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    // symlink_metadata, not metadata: we must see a symlink as a symlink to copy the link
    // rather than its target.
    let meta = fs::symlink_metadata(src)?;
    let file_type = meta.file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(src)?;
        std::os::unix::fs::symlink(target, dst)?;
        return Ok(());
    }

    if file_type.is_dir() {
        fs::create_dir(dst)?;
        // Directory permission preservation is best-effort: a filesystem that rejects the
        // mode change must not fail the whole copy of otherwise-good contents.
        let _ = fs::set_permissions(dst, meta.permissions());
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }

    // Regular file: fs::copy preserves the permission bits. `src` is known-not-a-symlink
    // here, so no symlink is dereferenced.
    fs::copy(src, dst)?;
    Ok(())
}

/// Remove `path` and, if it is a directory, its whole subtree. A symlink is unlinked as a
/// symlink (its target is never removed). Used by permanent delete and the EXDEV move
/// fallback.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_dir() {
        fs::remove_dir_all(path)
    } else {
        // Covers regular files and symlinks (including a symlink to a directory).
        fs::remove_file(path)
    }
}

/// Move `src` to `dst`, falling back to copy + delete on a cross-volume rename (SPEC §8,
/// point 5). Production path; the EXDEV branch is exercised by `move_item_with`.
pub fn move_item(src: &Path, dst: &Path) -> io::Result<()> {
    move_item_with(src, dst, |from, to| fs::rename(from, to))
}

/// The move core with an injectable rename, so tests force the EXDEV fallback without a
/// real second volume. On a cross-device error the source is copied then removed; a copy
/// failure aborts before the delete so nothing is lost.
fn move_item_with<R>(src: &Path, dst: &Path, rename: R) -> io::Result<()>
where
    R: Fn(&Path, &Path) -> io::Result<()>,
{
    match rename(src, dst) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(EXDEV) => {
            copy_tree(src, dst)?;
            remove_tree(src)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique temp directory removed on drop (no `tempfile` dependency, matching the
    /// other modules' test helpers).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "beeline_fsops_{}_{}",
                std::process::id(),
                unique
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn split_name_cases() {
        assert_eq!(split_name("report.pdf"), ("report", ".pdf"));
        assert_eq!(split_name("notes"), ("notes", ""));
        // A leading dot is part of the stem, not an extension delimiter.
        assert_eq!(split_name(".gitignore"), (".gitignore", ""));
        // Only the last dot splits, so multi-part extensions bump on the stem.
        assert_eq!(split_name("archive.tar.gz"), ("archive.tar", ".gz"));
    }

    #[test]
    fn collision_suffix_chain() {
        let dir = TempDir::new();
        // No collision → the bare name.
        assert_eq!(
            resolve_collision(dir.path(), "a.txt"),
            dir.path().join("a.txt")
        );

        // Occupy the chain and watch the suffix climb 2 → 3.
        fs::write(dir.path().join("a.txt"), b"x").unwrap();
        assert_eq!(
            resolve_collision(dir.path(), "a.txt"),
            dir.path().join("a 2.txt")
        );
        fs::write(dir.path().join("a 2.txt"), b"x").unwrap();
        assert_eq!(
            resolve_collision(dir.path(), "a.txt"),
            dir.path().join("a 3.txt")
        );

        // Extension-less names suffix without a trailing dot.
        fs::write(dir.path().join("notes"), b"x").unwrap();
        assert_eq!(
            resolve_collision(dir.path(), "notes"),
            dir.path().join("notes 2")
        );
    }

    #[test]
    fn recursive_copy_preserves_symlink_without_following() {
        let dir = TempDir::new();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("file.txt"), b"hello").unwrap();
        fs::create_dir(src.join("nested")).unwrap();
        fs::write(src.join("nested/deep.txt"), b"deep").unwrap();
        // A relative symlink pointing at a sibling file inside the tree.
        symlink("file.txt", src.join("link.txt")).unwrap();

        let dst = dir.path().join("dst");
        copy_tree(&src, &dst).unwrap();

        assert_eq!(fs::read(dst.join("file.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dst.join("nested/deep.txt")).unwrap(), b"deep");
        // The copied link is still a symlink (not followed into a regular file)...
        let link_meta = fs::symlink_metadata(dst.join("link.txt")).unwrap();
        assert!(link_meta.file_type().is_symlink());
        // ...and it kept its original target verbatim.
        assert_eq!(
            fs::read_link(dst.join("link.txt")).unwrap(),
            Path::new("file.txt")
        );
    }

    #[test]
    fn exdev_move_falls_back_to_copy_and_delete() {
        let dir = TempDir::new();
        let src = dir.path().join("moving.txt");
        fs::write(&src, b"payload").unwrap();
        let dst = dir.path().join("moved.txt");

        // Force the cross-volume branch with a rename that always reports EXDEV.
        move_item_with(&src, &dst, |_, _| Err(io::Error::from_raw_os_error(EXDEV))).unwrap();

        assert_eq!(fs::read(&dst).unwrap(), b"payload");
        // The source is gone: copy succeeded, then the fallback removed it.
        assert!(!path_exists(&src));
    }

    #[test]
    fn exdev_move_of_directory() {
        let dir = TempDir::new();
        let src = dir.path().join("tree");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("inner.txt"), b"z").unwrap();
        let dst = dir.path().join("tree-moved");

        move_item_with(&src, &dst, |_, _| Err(io::Error::from_raw_os_error(EXDEV))).unwrap();

        assert_eq!(fs::read(dst.join("inner.txt")).unwrap(), b"z");
        assert!(!path_exists(&src));
    }

    #[test]
    fn remove_tree_unlinks_symlink_not_target() {
        let dir = TempDir::new();
        let target = dir.path().join("real.txt");
        fs::write(&target, b"keep").unwrap();
        let link = dir.path().join("ptr.txt");
        symlink(&target, &link).unwrap();

        remove_tree(&link).unwrap();
        assert!(!path_exists(&link));
        // Removing the link must not remove what it pointed at.
        assert!(path_exists(&target));
    }
}
