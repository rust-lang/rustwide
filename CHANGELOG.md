# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- New variant `PrepareError::MissingDependencies`, returned during the prepare
  step when a dependency does not exist.

### Changed

- Path dependencies are no longer removed from `Cargo.toml` during the prepare
  step.

## [0.11.1] - 2021-01-25

### Changed

* Updated tokio dependency to 1.0.

## [0.11.0] - 2020-10-30

### Added

- New method `Crate::registry` to use crates from alternative registries.

### Changed

- Allow workspaces by having `validate_manifest` use `metadata --no-deps`
  instead of deprecated `read-manifest`, therefor no longer failing on
  workspaces and `TomlTweaker` no longer removing the workspace table from
  `Cargo.toml`.
- `Command` now warns when it is not used.
- Errors while removing directories or files now mentions the path that caused
  the error to happen.

## [0.10.0] - 2020-08-08

### Added

- New variant `CommandError::ExecutionFailed`
- New variant `CommandError::KillAfterTimeoutFailed`
- New variant `CommandError::SandboxImagePullFailed`
- New variant `CommandError::SandboxImageMissing`
- New variant `CommandError::WorkspaceNotMountedCorrectly`
- New variant `CommandError::InvalidDockerInspectOutput`
- New variant `CommandError::IO`
- New struct `KillFailedError`

### Changed

- **BREAKING**: support for CI toolchains is now gated behind the
  `unstable-toolchain-ci` Cargo feature.
- **BREAKING**: all functions and methods inside `cmd` now return `CommandError`.
- `winapi` is no longer required on unix; `nix` is no longer required on windows.
- Relaxed lifetime restrictions of `Build::cmd` and `Build::cargo`.
- The requirement of using an image similar to `crates-build-env` has been
  lifted, and it's now possible to use any Docker image for the sandbox.

## [0.9.0] - 2020-07-01

### Added

- New method `Toolchain::remove_component`

### Fixed

- When passed a global command with the same name as a file in the current directory,
  Rustwide will now execute the global command instead of the file.

## [0.8.0] - 2020-06-05

### Added

- New method `Workspace::purge_all_caches`.

### Changed

- The exact image has used during builds will be logged.

### Fixed

- Subcommands executed in sandbox respect configs of parent command.

## [0.7.1] - 2020-05-20

### Changed

- Updated dependencies.

## [0.7.0] - 2020-05-07

### Added

- New struct `cmd::ProcessLinesActions` to expose actions available while reading live output from a command.

### Changed

- **BREAKING**: `Command::process_lines` now accepts a `FnMut(&str, &mut ProcessLinesActions)`.
- The file `.cargo/config` will be removed before starting the build.

## [0.6.1] - 2020-05-04

### Fixed

- Fix `Command::process_lines` not working in sandboxed enviroments.

## [0.6.0] - 2020-04-01

### Added

- New method `SandboxBuilder::limit_cpu`

## [0.5.1] - 2020-01-31

### Fixed

- Fix `unstable` feature not working after the Rust 1.41.0 stable release.

## [0.5.0] - 2019-12-30

### Added

- New enum `toolchain::ToolchainError`
- New method `Toolchain::remove_target`
- New method `Toolchain::installed_targets`

## [0.4.0] - 2019-12-23

### Added

- New struct `toolchain::CiToolchain` containing a CI toolchain's metadata.
- New struct `toolchain::DistToolchain` containing a dist toolchain's metadata.
- New method `WorkspaceBuilder::rustup_profile` to configure the rustup profile
  used during builds.
- New method `Toolchain::as_ci` to get a CI toolchain's metadata.
- New method `Toolchain::as_dist` to get a dist toolchain's metadata.
- New method `Toolchain::ci` to create CI toolchains.
- New method `Toolchain::dist` to create dist toolchains.

### Changed

- **BREAKING**: The default rustup profile is now `minimal`.
- **BREAKING**: The `Toolchain` enum is now an opaque struct.
- The directory `target/` inside local crates won't be copied into the build
  anymore.
- Symbolic links will be followed instead of copied as links.

### Fixed

- Copying broken symbolic links will now include the path of the link in the
  error message.
- Fix removing present standalone tests during TOML tweaks.

## [0.3.2] - 2019-10-08

### Fixed

- The default value for `WorkspaceBuilder::fetch_registry_index_during_builds`
  was mistakenly set to `false` instead of `true` by default.

## [0.3.1] - 2019-09-23

### Fixed

- Building Rustwide failed on Windows due to a missing feature flag on the
  getrandom crate.

## [0.3.0] - 2019-09-23

### Added

- New method `Toolchain::rustc` to execute a toolchain's `rustc`.
- New method `WorkspaceBuilder::fetch_registry_index_during_builds` to enable
  or disable fetching the registry's index during each build. The method is
  only available when the `unstable` rustwide feature is enabled.
- New method `Crate::purge_from_cache` to remove the cached copy of a crate.
- New method `BuildBuilder::patch_with_git` to replace crates.
- New method `BuildBuilder::run` to run a build.
- New method `Command::log_command` to disable logging the command name and
  args before executing it.
- New method `WorkspaceBuilder::running_inside_docker` to adapt Rustwide itself
  to run inside a Docker container.

### Changed

- **BREAKING:** The registry index will now be fetched during each build
  instead of being cached during the workspace's initialization. It's possible
  to use the `WorkspaceBuilder::fetch_registry_index_during_builds` method to
  revert to the old behavior.
- **BREAKING:** The `BuildDirectory::build` method now returns an instance of
  `BuildBuilder`. You'll need to then call `BuildBuilder::run` to restore the
  old behavior.
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

[0.11.1]: https://github.com/rust-lang/rustwide/releases/tag/0.11.1
[0.11.0]: https://github.com/rust-lang/rustwide/releases/tag/0.11.0
[0.10.0]: https://github.com/rust-lang/rustwide/releases/tag/0.10.0
[0.9.0]: https://github.com/rust-lang/rustwide/releases/tag/0.9.0
[0.8.0]: https://github.com/rust-lang/rustwide/releases/tag/0.8.0
[0.7.1]: https://github.com/rust-lang/rustwide/releases/tag/0.7.1
[0.7.0]: https://github.com/rust-lang/rustwide/releases/tag/0.7.0
[0.6.1]: https://github.com/rust-lang/rustwide/releases/tag/0.6.1
[0.6.0]: https://github.com/rust-lang/rustwide/releases/tag/0.6.0
[0.5.1]: https://github.com/rust-lang/rustwide/releases/tag/0.5.1
[0.5.0]: https://github.com/rust-lang/rustwide/releases/tag/0.5.0
[0.4.0]: https://github.com/rust-lang/rustwide/releases/tag/0.4.0
[0.3.2]: https://github.com/rust-lang/rustwide/releases/tag/0.3.2
[0.3.1]: https://github.com/rust-lang/rustwide/releases/tag/0.3.1
[0.3.0]: https://github.com/rust-lang/rustwide/releases/tag/0.3.0
[0.2.0]: https://github.com/rust-lang/rustwide/releases/tag/0.2.0
[0.1.0]: https://github.com/rust-lang/rustwide/releases/tag/0.1.0
