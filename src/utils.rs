use crate::PrepareError;
use log::{info, warn};
use percent_encoding::{AsciiSet, CONTROLS};
use std::fs::{self, OpenOptions, TryLockError};
use std::path::{Component, Path, PathBuf, Prefix, PrefixComponent};
use walkdir::WalkDir;

const ENCODE_SET: AsciiSet = CONTROLS
    .add(b'/')
    .add(b'\\')
    .add(b'<')
    .add(b'>')
    .add(b':')
    .add(b'"')
    .add(b'|')
    .add(b'?')
    .add(b'*')
    .add(b' ');

pub(crate) fn escape_path(unescaped: &[u8]) -> String {
    percent_encoding::percent_encode(unescaped, &ENCODE_SET).to_string()
}

pub(crate) fn file_lock<T>(
    path: &Path,
    msg: &str,
    f: impl FnOnce() -> anyhow::Result<T> + std::panic::UnwindSafe,
) -> anyhow::Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;

    match file.try_lock() {
        Ok(_) => {}
        Err(TryLockError::WouldBlock) => {
            warn!("blocking on other processes finishing to {msg}");
            file.lock()?;
        }
        Err(err) => return Err(err.into()),
    }

    let res = std::panic::catch_unwind(f);
    #[allow(unstable_name_collisions)]
    // the method is coming from the `fs2` crate
    // https://github.com/danburkert/fs2-rs/issues/50
    let _ = file.unlock();

    match res {
        Ok(res) => res,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// If a prefix uses the extended-length syntax (`\\?\`), return the equivalent version without it.
///
/// Returns `None` if `prefix.kind().is_verbatim()` is `false`.
fn strip_verbatim_from_prefix(prefix: &PrefixComponent<'_>) -> Option<PathBuf> {
    let ret = match prefix.kind() {
        Prefix::Verbatim(s) => Path::new(s).to_owned(),

        Prefix::VerbatimDisk(drive) => [format!(r"{}:\", drive as char)].iter().collect(),

        Prefix::VerbatimUNC(_, _) => unimplemented!(),

        _ => return None,
    };

    Some(ret)
}

pub(crate) fn remove_file(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path).map_err(|error| crate::utils::improve_remove_error(error, path))
}

#[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
pub(crate) fn remove_dir_all(path: &Path) -> std::io::Result<()> {
    remove_dir_all::remove_dir_all(path)
        .map_err(|error| crate::utils::improve_remove_error(error, path))
}

#[derive(Debug)]
struct RemoveError {
    underlying: std::io::Error,
    path: PathBuf,
}

impl std::error::Error for RemoveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.underlying)
    }
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "failed to remove '{}' : {:?}",
            self.path.display(),
            self.underlying
        ))
    }
}

fn improve_remove_error(error: std::io::Error, path: &Path) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        RemoveError {
            underlying: error,
            path: path.to_path_buf(),
        },
    )
}

/// Remove dangling symlinks inside `base`, and fail with
/// [`PrepareError::PathEscapesSource`] on a symlink that points outside of
/// `base` or can't be resolved.
///
/// The source is used on the host, outside the sandbox, so following such a
/// symlink could read, write or delete host files. Registry crates can't
/// contain symlinks, but git crates can.
///
/// Dangling symlinks are removed instead of reported: they are common (e.g.
/// links into submodules), but writing through them would create the target.
pub(crate) fn sanitize_symlinks(base: &Path) -> anyhow::Result<()> {
    use std::io::ErrorKind;

    let canonical_base = match fs::canonicalize(base) {
        Ok(path) => path,
        // The caller reports the missing source.
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in WalkDir::new(base).follow_links(false) {
        let entry = entry?;
        if !entry.path_is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .expect("walkdir paths start with the root");

        match fs::canonicalize(path) {
            Ok(resolved) if resolved.starts_with(&canonical_base) => {}
            Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                // On Windows, directory symlinks have to be removed as directories.
                if let Err(err) = remove_file(path) {
                    fs::remove_dir(path).map_err(|_| err)?;
                }
                // Crate-controlled path, so escape it.
                info!("removed dangling symlink {relative:?}");
            }
            Ok(_) | Err(_) => {
                return Err(PrepareError::PathEscapesSource(relative.to_path_buf()).into());
            }
        }
    }
    Ok(())
}

pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // `fs::canonicalize` returns an extended-length path on Windows. Such paths not supported by
    // many programs, including rustup. We strip the `\\?\` prefix of the canonicalized path, but
    // this changes the meaning of some path components, and imposes a length of around 260
    // characters.
    if cfg!(windows) {
        // A conservative estimate for the maximum length of a path on Windows.
        //
        // The additional 12 byte restriction is applied when creating directories. It ensures that
        // files can always be created inside that directory without exceeding the path limit.
        const MAX_PATH_LEN: usize = 260 - 12;

        let mut components = p.components();
        let first_component = components.next().unwrap();

        if let Component::Prefix(prefix) = first_component
            && let Some(mut modified_path) = strip_verbatim_from_prefix(&prefix)
        {
            modified_path.push(components.as_path());
            p = modified_path;
        }

        if p.as_os_str().len() >= MAX_PATH_LEN {
            warn!(
                "Canonicalized path is too long for Windows: {:?}",
                p.as_os_str(),
            );
        }
    }

    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_remove_error() {
        let path = "test/path".as_ref();

        let expected = "failed to remove 'test/path' : Kind(PermissionDenied)";
        let tested = format!(
            "{}",
            improve_remove_error(
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                path
            )
        );
        assert_eq!(expected, tested);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod escaping_symlink_tests {
    use super::sanitize_symlinks;
    use crate::PrepareError;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    /// A source tree like a typical crate, with a workspace member.
    fn source_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"").unwrap();
        fs::write(dir.path().join("LICENSE"), b"").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("lib.rs"), b"").unwrap();
        fs::create_dir_all(dir.path().join("member")).unwrap();
        dir
    }

    /// A directory outside of the source tree, containing a `secret` file.
    fn host_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secret"), b"token").unwrap();
        dir
    }

    /// The path reported by `PrepareError::PathEscapesSource`, if any.
    fn escaping(base: &Path) -> Option<std::path::PathBuf> {
        let err = sanitize_symlinks(base).err()?;
        match err.downcast_ref::<PrepareError>() {
            Some(PrepareError::PathEscapesSource(path)) => Some(path.clone()),
            _ => panic!("unexpected error: {err:?}"),
        }
    }

    #[test]
    fn missing_source_directory() {
        let src = source_tree();
        assert_eq!(escaping(&src.path().join("does-not-exist")), None);
    }

    #[test]
    fn in_tree_symlinks_are_allowed() {
        let src = source_tree();
        // relative link climbing up, but staying inside the tree
        symlink("../LICENSE", src.path().join("member").join("LICENSE")).unwrap();
        // absolute link into the tree
        symlink(src.path().join("src"), src.path().join("src-link")).unwrap();
        // chain of in-tree links
        symlink("src-link/lib.rs", src.path().join("lib-link.rs")).unwrap();
        assert_eq!(escaping(src.path()), None);
    }

    #[test]
    fn relative_symlink_climbing_out_of_the_tree() {
        let host = host_dir();
        let src = source_tree();
        // `host` and `src` are siblings in the temp dir
        let target = Path::new("..")
            .join("..")
            .join(host.path().file_name().unwrap())
            .join("secret");
        symlink(target, src.path().join("member").join("Cargo.toml")).unwrap();
        assert_eq!(
            escaping(src.path()),
            Some(Path::new("member").join("Cargo.toml"))
        );
    }

    #[test]
    fn chain_of_symlinks_ending_outside_the_tree() {
        let host = host_dir();
        let src = source_tree();
        symlink("zz-last", src.path().join("Cargo.lock")).unwrap();
        symlink(host.path().join("secret"), src.path().join("zz-last")).unwrap();
        // both links resolve outside, whichever the walk sees first
        let found = escaping(src.path()).unwrap();
        assert!(
            found == Path::new("Cargo.lock") || found == Path::new("zz-last"),
            "unexpected path: {found:?}"
        );
    }

    #[test]
    fn dangling_symlinks_are_removed() {
        let host = host_dir();
        let src = source_tree();
        let links = [
            // e.g. a link into a submodule that isn't checked out
            ("LICENSE-sub", "member/sub/LICENSE".into()),
            // chain of dangling links
            ("chained", "LICENSE-sub".into()),
            // through a file, which can't be traversed
            ("through-file", "Cargo.toml/file".into()),
            // host-side writes through these would create a host file
            ("Cargo.lock", host.path().join("new-file")),
            ("member/Cargo.lock", "../../does-not-exist".into()),
        ];
        for (link, target) in &links {
            symlink(target, src.path().join(link)).unwrap();
        }
        // a resolvable link is kept
        symlink("LICENSE", src.path().join("LICENSE-link")).unwrap();

        assert_eq!(escaping(src.path()), None);

        for (link, _) in &links {
            assert!(
                fs::symlink_metadata(src.path().join(link)).is_err(),
                "{link} should have been removed"
            );
        }
        assert!(src.path().join("LICENSE-link").is_symlink());
        assert!(!host.path().join("new-file").exists());
    }

    #[test]
    fn looping_symlinks() {
        let src = source_tree();
        symlink("loop-b", src.path().join("loop-a")).unwrap();
        symlink("loop-a", src.path().join("loop-b")).unwrap();
        let found = escaping(src.path()).unwrap();
        assert!(
            found == Path::new("loop-a") || found == Path::new("loop-b"),
            "unexpected path: {found:?}"
        );
    }
}

#[cfg(test)]
#[cfg(windows)]
mod windows_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn strip_verbatim() {
        let suite = vec![
            (r"C:\Users\carl", None),
            (r"\Users\carl", None),
            (r"\\?\C:\Users\carl", Some(r"C:\")),
            (r"\\?\Users\carl", Some(r"Users")),
        ];

        for (input, output) in suite {
            let p = Path::new(input);
            let first_component = p.components().next().unwrap();

            if let Component::Prefix(prefix) = &first_component {
                let stripped = strip_verbatim_from_prefix(prefix);
                assert_eq!(stripped.as_ref().map(|p| p.to_str().unwrap()), output);
            } else {
                assert!(output.is_none());
            }
        }
    }
}
