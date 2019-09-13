# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- New method `Toolchain::rustc` to execute a toolchain's `rustc`.
- New method `WorkspaceBuilder::fetch_registry_index_during_builds` to enable
  or disable fetching the registry's index during each build. The method is
  only available when the `unstable` rustwide feature is enabled.
- New method `Crate::purge_from_cache` to remove the cached copy of a crate.

### Changed

- **BREAKING:** The registry index will now be fetched during each build
  instead of being cached during the workspace's initialization. It's possible
  to use the `WorkspaceBuilder::fetch_registry_index_during_builds` method to
  revert to the old behavior.
- When the `unstable` feature flag is enabled rustwide will use Cargo's
  `-Zinstall-upgrade` to update its tools instead of the
  `cargo-install-upgrade` crate. This will speed up the workspace
  initialization.

### Fixed

- Calling `Workspace::purge_all_build_dirs` returned an error when no
  directories were present instead of doing nothing.

## [0.2.0] - 2019-09-06

### Added

- New method `Toolchain::uninstall` to remove a previously installed toolchain.
- New method `Workspace::installed_toolchains` to get a list of all the
  toolchains in the workspace.
- New error `PrepareError::PrivateGitRepository` when `Crate::fetch` is called
  on a private or missing git repository.

### Changed

- **BREAKING:** The `cmd::Binary` enum is not exaustive anymore.
- **BREAKING:** The `cmd::MountKind` enum is not exaustive anymore.
- **BREAKING:** The `cmd::Toolchain` enum is not exaustive anymore.
- The base path of mounts inside the sandbox is now `/opt/rustwide` on Linux
  and `C:\rustwide` on Windows.

### Fixed

- Cloning git repositories on windows hanged due to the credential manager.
- Cleanups were failing on Windows due to permission errors.
- Cached git repositories weren't updated after the initial clone.

## [0.1.0] - 2019-08-22

### Added

- Initial version of Rustwide, extracted from Crater.

[0.2.0]: https://github.com/rust-lang/rustwide/releases/tag/0.2.0
[0.1.0]: https://github.com/rust-lang/rustwide/releases/tag/0.1.0
