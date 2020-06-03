use failure::Error;
use rustwide::cmd::SandboxBuilder;
use rustwide::{Crate, Toolchain};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const WORKSPACE_NAME: &str = "purge-caches";

#[test]
fn test_purge_caches() -> Result<(), Error> {
    let workspace_path = crate::utils::workspace_path(WORKSPACE_NAME);
    let workspace = crate::utils::init_named_workspace(WORKSPACE_NAME)?;

    // Do an initial purge to prevent stale files from being present.
    workspace.purge_all_build_dirs()?;
    workspace.purge_all_caches()?;

    let toolchain = Toolchain::dist("stable");
    toolchain.install(&workspace)?;

    let start_contents = WorkspaceContents::collect(&workspace_path)?;

    let crates = vec![
        Crate::crates_io("lazy_static", "1.0.0"),
        Crate::git("https://github.com/pietroalbini/git-credential-null"),
    ];

    // Simulate a build, which is going to fill up the caches.
    for krate in &crates {
        krate.fetch(&workspace)?;

        let sandbox = SandboxBuilder::new().enable_networking(false);
        let mut build_dir = workspace.build_dir("shared");
        build_dir.build(&toolchain, krate, sandbox).run(|build| {
            build.cargo().args(&["check"]).run()?;
            Ok(())
        })?;
    }

    // After all the builds are done purge everything again, and ensure the contents are the same
    // as when we started.
    workspace.purge_all_build_dirs()?;
    workspace.purge_all_caches()?;
    let end_contents = WorkspaceContents::collect(&workspace_path)?;
    start_contents.assert_same(end_contents);

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct WorkspaceContents {
    files: HashMap<PathBuf, Digest>,
}

impl WorkspaceContents {
    fn collect(path: &Path) -> Result<Self, Error> {
        let mut files = HashMap::new();

        for entry in walkdir::WalkDir::new(path) {
            let entry = entry?;
            if !entry.metadata()?.is_file() {
                continue;
            }

            let mut sha = Sha1::new();
            sha.update(&std::fs::read(entry.path())?);

            files.insert(entry.path().into(), sha.digest());
        }

        Ok(Self { files })
    }

    fn assert_same(self, mut other: Self) {
        let mut same = true;

        println!("=== start directory differences ===");

        for (path, start_digest) in self.files.into_iter() {
            if let Some(end_digest) = other.files.remove(&path) {
                if start_digest != end_digest {
                    println!("file {} changed", path.display());
                    same = false;
                }
            } else {
                println!("file {} was removed", path.display());
                same = false;
            }
        }

        for (path, _) in other.files.into_iter() {
            println!("file {} was added", path.display());
            same = false;
        }

        println!("=== end directory differences ===");

        if !same {
            panic!("the contents of the directory changed");
        }
    }
}
