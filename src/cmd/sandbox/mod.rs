mod docker;

#[cfg(test)]
use crate::cmd::sandbox::docker::HostCgroup;
use crate::{
    Workspace,
    cmd::{Command, CommandError, ProcessLinesActions, ProcessOutput, container_dirs},
};
use docker::CgroupStatsReader;
use log::{error, info};
use serde::Deserialize;
use std::{
    cell::RefCell,
    ffi::OsString,
    fmt, mem,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    rc::Rc,
    str,
    time::Duration,
};

/// The Docker image used for sandboxing.
#[derive(Debug)]
pub struct SandboxImage {
    name: String,
}

impl SandboxImage {
    /// Load a local image present in the host machine.
    ///
    /// If the image is not available locally an error will be returned instead.
    pub fn local(name: &str) -> Result<Self, CommandError> {
        let image = SandboxImage { name: name.into() };
        info!("sandbox image is local, skipping pull");
        image.ensure_exists_locally()?;
        Ok(image)
    }

    /// Pull an image from its Docker registry.
    ///
    /// This will access the network to download the image from the registry. If pulling fails an
    /// error will be returned instead.
    pub fn remote(name: &str) -> Result<Self, CommandError> {
        let mut image = SandboxImage { name: name.into() };
        info!("pulling image {name} from Docker Hub");
        Command::new_workspaceless("docker")
            .args(["pull", name])
            .run()
            .map_err(|e| CommandError::SandboxImagePullFailed(Box::new(e)))?;
        if let Some(name_with_hash) = image.get_name_with_hash() {
            image.name = name_with_hash;
            info!("pulled image {}", image.name);
        }
        image.ensure_exists_locally()?;
        Ok(image)
    }

    fn ensure_exists_locally(&self) -> Result<(), CommandError> {
        info!("checking the image {} is available locally", self.name);
        Command::new_workspaceless("docker")
            .args(["image", "inspect", &self.name])
            .log_output(false)
            .run()
            .map_err(|e| CommandError::SandboxImageMissing(Box::new(e)))?;
        Ok(())
    }

    /// Get the image name with its hash, if available.
    /// In case of a github package registry image, something like:
    ///    ghcr.io/rust-lang/crates-build-env/linux@sha256:61361fe0a...
    pub fn get_name_with_hash(&self) -> Option<String> {
        Command::new_workspaceless("docker")
            .args([
                "inspect",
                &self.name,
                "--format",
                "{{index .RepoDigests 0}}",
            ])
            .log_output(false)
            .run_capture()
            .ok()?
            .stdout_lines()
            .first()
            .cloned()
    }
}

/// Whether to mount a path in the sandbox with write permissions or not.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MountKind {
    /// Allow the sandboxed code to change the mounted data.
    ReadWrite,
    /// Prevent the sandboxed code from changing the mounted data.
    ReadOnly,
}

#[derive(Clone)]
struct MountConfig {
    host_path: PathBuf,
    sandbox_path: PathBuf,
    perm: MountKind,
}

impl MountConfig {
    fn host_path(&self, workspace: &Workspace) -> Result<PathBuf, CommandError> {
        if let Some(container) = workspace.current_container() {
            // If we're inside a Docker container we'll need to remap the mount sources to point to
            // the directories in the host system instead of the containers. To do that we try to
            // see if the mount source is inside an existing mount point, and "rebase" the path.
            let inside_container_path = crate::utils::normalize_path(&self.host_path);
            for mount in container.mounts() {
                let dest = crate::utils::normalize_path(Path::new(mount.destination()));
                if let Ok(shared) = inside_container_path.strip_prefix(&dest) {
                    return Ok(Path::new(mount.source()).join(shared));
                }
            }
            Err(CommandError::WorkspaceNotMountedCorrectly)
        } else {
            Ok(crate::utils::normalize_path(&self.host_path))
        }
    }

    fn to_volume_arg(&self, workspace: &Workspace) -> Result<String, CommandError> {
        let perm = match self.perm {
            MountKind::ReadWrite => "rw",
            MountKind::ReadOnly => "ro",
        };
        Ok(format!(
            "{}:{}:{},Z",
            self.host_path(workspace)?.to_string_lossy(),
            self.sandbox_path.to_string_lossy(),
            perm
        ))
    }

    fn to_mount_arg(&self, workspace: &Workspace) -> Result<String, CommandError> {
        let mut opts_with_leading_comma = vec![];

        if self.perm == MountKind::ReadOnly {
            opts_with_leading_comma.push(",readonly");
        }

        Ok(format!(
            "type=bind,src={},dst={}{}",
            self.host_path(workspace)?.to_string_lossy(),
            self.sandbox_path.to_string_lossy(),
            opts_with_leading_comma.join(""),
        ))
    }
}

/// The sandbox builder allows configuring a [`Sandbox`].
///
/// Call [`SandboxBuilder::start`] to create a live sandbox, then run commands
/// inside it with [`Command::new_in_sandbox`](struct.Command.html#method.new_in_sandbox).
#[derive(Clone)]
pub struct SandboxBuilder {
    mounts: Vec<MountConfig>,
    source_dir_mount_kind: MountKind,
    memory_limit: Option<usize>,
    cpu_limit: Option<f32>,
    cpuset_cpus: Option<RangeInclusive<usize>>,
    enable_networking: bool,
    docker_runtime: DockerRuntime,
}

/// The Docker runtime used for sandbox containers.
///
/// This controls Docker's `--runtime` option on sandbox container creation.
/// [`DockerRuntime::Default`] omits the option and lets the Docker daemon use
/// its configured default runtime.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DockerRuntime {
    /// Let Docker use the daemon's configured default runtime.
    ///
    /// This does not pass a `--runtime` argument to Docker.
    #[default]
    Default,

    /// Use gVisor's `runsc` runtime.
    ///
    /// This passes `--runtime runsc` to Docker.
    Runsc,
}

impl DockerRuntime {
    /// Name of the runtime for Docker's `--runtime` argument.
    fn docker_name(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Runsc => Some("runsc"),
        }
    }

    /// Whether the runtime exposes the host-managed cgroup files inside the
    /// sandbox container.
    ///
    /// If not, statistics must use host-level cgroup files.
    fn supports_cgroup_files_inside_container(&self) -> bool {
        match self {
            DockerRuntime::Default => true,
            DockerRuntime::Runsc => false,
        }
    }
}

impl fmt::Display for DockerRuntime {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Default => "default".fmt(f),
            Self::Runsc => "runsc".fmt(f),
        }
    }
}

impl str::FromStr for DockerRuntime {
    type Err = ParseDockerRuntimeError;

    /// Parse a Docker runtime name.
    ///
    /// Accepts `""` and `"default"` for [`DockerRuntime::Default`], and
    /// `"runsc"` for [`DockerRuntime::Runsc`].
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "" | "default" => Ok(Self::Default),
            "runsc" => Ok(Self::Runsc),
            _ => Err(ParseDockerRuntimeError),
        }
    }
}

/// Error returned when parsing an unsupported Docker runtime name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseDockerRuntimeError;

impl fmt::Display for ParseDockerRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        "unsupported Docker runtime".fmt(f)
    }
}

impl std::error::Error for ParseDockerRuntimeError {}

/// Statistics collected for a sandbox.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SandboxStatistics {
    memory_peak: Option<u64>,
}

impl SandboxStatistics {
    /// Return the peak memory usage in bytes observed across the whole sandbox, if available.
    pub fn memory_peak_bytes(&self) -> Option<u64> {
        self.memory_peak
    }

    /// Combine two `SandboxStatistics` into one, keeping the highest observed peak memory.
    pub fn combine(self, other: Self) -> Self {
        Self {
            memory_peak: match (self.memory_peak, other.memory_peak) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            },
        }
    }

    /// Merge another `SandboxStatistics` into `self` in place.
    pub fn merge(&mut self, other: Self) {
        *self = mem::take(self).combine(other);
    }
}

#[derive(Debug, Default)]
pub(crate) struct SandboxStatisticsState {
    statistics: RefCell<SandboxStatistics>,
}

impl SandboxStatisticsState {
    pub(crate) fn snapshot(&self) -> SandboxStatistics {
        self.statistics.borrow().clone()
    }

    fn merge(&self, statistics: SandboxStatistics) {
        self.statistics.borrow_mut().merge(statistics);
    }
}

/// A live sandbox that can execute one or more commands.
///
/// Sandboxes are returned already started by [`SandboxBuilder::start`] and
/// can be reused across multiple commands. If a command exhausts the
/// container's memory limit and kills the container, the next command
/// transparently recreates it.
pub struct Sandbox<'w> {
    workspace: &'w Workspace,
    builder: SandboxBuilder,
    source_dir: PathBuf,
    target_dir: PathBuf,
    container: Option<Container<'w>>,
    statistics: Rc<SandboxStatisticsState>,
}

pub(crate) struct SandboxCommand {
    pub(crate) cmd: Vec<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) workdir: Option<PathBuf>,
    pub(crate) user: Option<String>,
}

impl SandboxCommand {
    pub(crate) fn new(program: impl Into<OsString>) -> SandboxCommand {
        Self {
            cmd: vec![program.into()],
            env: Vec::new(),
            workdir: None,
            user: None,
        }
    }

    pub(crate) fn user(mut self, user: u32, group: u32) -> Self {
        self.user = Some(format!("{user}:{group}"));
        self
    }

    pub(crate) fn workdir(mut self, workdir: impl AsRef<Path>) -> Self {
        self.workdir = Some(crate::utils::normalize_path(workdir.as_ref()));
        self
    }

    pub(crate) fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub(crate) fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.cmd.push(arg.into());
        self
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        for arg in args {
            self = self.arg(arg);
        }
        self
    }
}

impl SandboxBuilder {
    /// Create a new sandbox builder.
    pub fn new() -> Self {
        Self {
            mounts: Vec::new(),
            source_dir_mount_kind: MountKind::ReadOnly,
            memory_limit: None,
            cpu_limit: None,
            cpuset_cpus: None,
            enable_networking: true,
            docker_runtime: DockerRuntime::default(),
        }
    }

    /// Mount a path inside the sandbox. It's possible to choose whether to mount the path
    /// read-only or writeable through the [`MountKind`](enum.MountKind.html) enum.
    pub fn mount(mut self, host_path: &Path, sandbox_path: &Path, kind: MountKind) -> Self {
        self.mounts.push(MountConfig {
            host_path: host_path.into(),
            sandbox_path: sandbox_path.into(),
            perm: kind,
        });
        self
    }

    /// Sets how the source directory is mounted for reusable sandbox commands.
    ///
    /// The default mount kind is read-only.
    ///
    /// ## Security
    ///
    /// Be sure you understand the implications of setting this. The same container
    /// backs every command spawned inside a single
    /// [`BuildBuilder::run`](../build/struct.BuildBuilder.html#method.run) closure, so with
    /// `MountKind::ReadWrite` any mutation made by an earlier command persists into all
    /// later commands in that build — and across reuse of the same source directory by
    /// later builds. Do not trust the source directory's contents to be untouched if you
    /// opt in to a writable mount.
    pub fn source_dir_mount_kind(mut self, mount_kind: MountKind) -> Self {
        self.source_dir_mount_kind = mount_kind;
        self
    }

    /// Enable or disable the sandbox's memory limit. When the processes inside the sandbox use
    /// more memory than the limit the sandbox will be killed.
    ///
    /// By default no memory limit is present, and its size is provided in bytes.
    pub fn memory_limit(mut self, limit: Option<usize>) -> Self {
        self.memory_limit = limit;
        self
    }

    /// Enable or disable the sandbox's CPU limit. The value of the limit is the fraction of CPU
    /// cores the sandbox is allowed to use.
    ///
    /// For example, on a 4-core machine, setting a CPU limit of `2.0` will only allow two of the
    /// cores to be used, while a CPU limit of `0.5` will only allow half of a single CPU core to
    /// be used.
    pub fn cpu_limit(mut self, limit: Option<f32>) -> Self {
        self.cpu_limit = limit;
        self
    }

    /// Restrict the sandbox to run on a specific inclusive range of CPU IDs.
    ///
    /// For example, `0..=1` will restrict the sandbox to CPUs 0 and 1 and translate to Docker's
    /// `--cpuset-cpus 0-1`.
    pub fn cpuset_cpus(mut self, cpus: Option<RangeInclusive<usize>>) -> Self {
        self.cpuset_cpus = cpus;
        self
    }

    /// Enable or disable the sandbox's networking. When it's disabled processes inside the sandbox
    /// won't be able to reach network service on the Internet or the host machine.
    ///
    /// By default networking is enabled.
    pub fn enable_networking(mut self, enable: bool) -> Self {
        self.enable_networking = enable;
        self
    }

    /// Use a specific Docker runtime for the sandbox container.
    ///
    /// [`DockerRuntime::Runsc`] maps to Docker's `--runtime runsc` flag. By
    /// default, [`DockerRuntime::Default`] is used and no runtime is passed, so
    /// Docker uses the daemon's configured default runtime.
    pub fn docker_runtime(mut self, runtime: DockerRuntime) -> Self {
        self.docker_runtime = runtime;
        self
    }

    /// Start a live sandbox from this configuration.
    ///
    /// The returned sandbox can be used to run one or more commands against a
    /// fixed source directory and target directory. The underlying container is
    /// created and started before this returns, so any docker errors surface here
    /// rather than on the first command.
    pub fn start<'w>(
        self,
        workspace: &'w Workspace,
        source_dir: impl AsRef<Path>,
        target_dir: impl AsRef<Path>,
    ) -> Result<Sandbox<'w>, CommandError> {
        self.start_with_statistics(
            workspace,
            source_dir,
            target_dir,
            Rc::new(SandboxStatisticsState::default()),
        )
    }

    pub(crate) fn start_with_statistics<'w>(
        self,
        workspace: &'w Workspace,
        source_dir: impl AsRef<Path>,
        target_dir: impl AsRef<Path>,
        statistics: Rc<SandboxStatisticsState>,
    ) -> Result<Sandbox<'w>, CommandError> {
        let source_dir = crate::utils::normalize_path(source_dir.as_ref());
        let target_dir = crate::utils::normalize_path(target_dir.as_ref());
        let container = Sandbox::create_container(&self, workspace, &source_dir, &target_dir)?;
        Ok(Sandbox {
            workspace,
            builder: self,
            source_dir,
            target_dir,
            container: Some(container),
            statistics,
        })
    }

    fn create_started(self, workspace: &Workspace) -> Result<Container<'_>, CommandError> {
        let mut container = self.create(workspace)?;
        container.start()?;
        container.refresh_state(&container.inspect()?);
        container.cgroup.detect_host_cgroup();
        container.cgroup.record_oom_kill_count();
        Ok(container)
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            skip_all,
            fields(
                image = %workspace.sandbox_image().name,
                mounts = self.mounts.len(),
                memory_limit = ?self.memory_limit,
                cpu_limit = ?self.cpu_limit,
                cpuset_cpus = ?self.cpuset_cpus,
                enable_networking = self.enable_networking,
                docker_runtime = ?self.docker_runtime,
            )
        )
    )]
    fn create(self, workspace: &Workspace) -> Result<Container<'_>, CommandError> {
        let mut args: Vec<String> = vec!["create".into()];

        // Mounts are container-level config, always on `docker create`
        for mount in &self.mounts {
            std::fs::create_dir_all(&mount.host_path)?;

            // On Windows, we mount paths containing a colon which don't work with `-v`, but on
            // Linux we need the Z flag, which doesn't work with `--mount`, for SELinux relabeling.
            if cfg!(windows) {
                args.push("--mount".into());
                args.push(mount.to_mount_arg(workspace)?)
            } else {
                args.push("-v".into());
                args.push(mount.to_volume_arg(workspace)?)
            }
        }

        // Resource limits and networking are container-level config
        if let Some(limit) = self.memory_limit {
            args.push("-m".into());
            args.push(limit.to_string());
        }

        if let Some(limit) = self.cpu_limit {
            args.push("--cpus".into());
            args.push(limit.to_string());
        }

        if let Some(cpus) = self.cpuset_cpus {
            args.push("--cpuset-cpus".into());
            args.push(format_cpuset_cpus(&cpus));
        }

        if !self.enable_networking {
            args.push("--network".into());
            args.push("none".into());
        }

        if cfg!(windows) {
            args.push("--isolation=process".into());
        }

        if let Some(runtime) = self.docker_runtime.docker_name() {
            args.push("--runtime".into());
            args.push(runtime.into());
        }

        args.push(workspace.sandbox_image().name.clone());

        // Use an idle command; the real command runs via `docker exec` so the container stays
        // alive after the command finishes, allowing us to read cgroup metrics.
        args.push("sleep".into());
        args.push("infinity".into());

        let out = Command::new(workspace, "docker")
            .args(&args)
            .run_capture()
            .map_err(|err| CommandError::SandboxContainerCreate(Box::new(err)))?;
        let id = out.stdout_lines().first().cloned().unwrap_or_default();
        Ok(Container {
            id: Some(id.clone()),
            workspace,
            running: true,
            oom_killed: false,
            cgroup: CgroupStatsReader::new(workspace, id, self.docker_runtime),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectContainer {
    state: InspectState,
}

#[derive(Deserialize)]
struct InspectState {
    #[serde(rename = "OOMKilled")]
    oom_killed: bool,
    #[serde(rename = "Pid")]
    pid: u32,
    #[serde(rename = "Running")]
    running: bool,
}

struct Container<'w> {
    /// Docker container ID. `Some` while the container is live; `take`n by a
    /// successful [`Container::delete`] so that [`Drop`] knows there's
    /// nothing left to remove.
    id: Option<String>,
    workspace: &'w Workspace,
    running: bool,
    oom_killed: bool,
    cgroup: CgroupStatsReader<'w>,
}

impl Container<'_> {
    fn id(&self) -> &str {
        self.id
            .as_deref()
            .expect("container has already been deleted")
    }
}

impl fmt::Display for Container<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.id().fmt(f)
    }
}

impl Container<'_> {
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    fn inspect(&self) -> Result<InspectContainer, CommandError> {
        let output = Command::new(self.workspace, "docker")
            .args(["inspect", self.id()])
            .log_output(false)
            .run_capture()?;

        let mut data: Vec<InspectContainer> =
            ::serde_json::from_str(&output.stdout_lines().join("\n"))
                .map_err(CommandError::InvalidDockerInspectOutput)?;
        assert_eq!(data.len(), 1);
        Ok(data.pop().unwrap())
    }

    /// Start the container in detached mode (without `-a`).
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    fn start(&self) -> Result<(), CommandError> {
        Command::new(self.workspace, "docker")
            .args(["start", self.id()])
            .log_output(false)
            .run()
            .map(|_| ())
    }

    fn refresh_state(&mut self, details: &InspectContainer) {
        self.running = details.state.running;
        self.cgroup.pid = Some(details.state.pid);
    }

    fn check_container_oom(&mut self, details: &InspectContainer) -> bool {
        self.refresh_state(details);
        // `OOMKilled` can stay true after the first failure. Treat it as an
        // edge-triggered signal so later commands in the same container don't
        // keep being reported as fresh OOMs.
        let previous = self.oom_killed;
        self.oom_killed = details.state.oom_killed;
        details.state.oom_killed && !previous
    }

    fn is_running(&self) -> bool {
        self.running
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(skip_all, fields(container_id = %self.id(), capture))
    )]
    fn run_command(
        &mut self,
        command: SandboxCommand,
        timeout: Option<Duration>,
        no_output_timeout: Option<Duration>,
        process_lines: Option<&mut dyn FnMut(&str, &mut ProcessLinesActions)>,
        log_output: bool,
        log_command: bool,
        capture: bool,
    ) -> (SandboxStatistics, Result<ProcessOutput, CommandError>) {
        // Build the `docker exec` command with env/workdir/user from the sandbox config
        let mut cmd = Command::new(self.workspace, "docker").arg("exec");

        for (var, value) in command.env {
            cmd = cmd
                .arg("-e")
                .arg(format!("{}={}", var.display(), value.display()));
        }

        if let Some(workdir) = command.workdir {
            cmd = cmd.arg("-w").arg(workdir);
        }

        if let Some(user) = command.user {
            cmd = cmd.arg("--user").arg(user);
        }

        cmd = cmd
            .arg(self.id())
            .args(command.cmd)
            .timeout(timeout)
            .log_output(log_output)
            .log_command(log_command)
            .no_output_timeout(no_output_timeout);

        if let Some(f) = process_lines {
            cmd = cmd.process_lines(f);
        }

        let res = cmd.run_inner(capture);

        // Read peak memory usage while the container is still running (best-effort)
        let statistics = SandboxStatistics {
            memory_peak: self.cgroup.read_memory_peak(),
        };

        // Check OOM via cgroup events (catches cases where only the exec'd process
        // was killed, leaving the container's init process alive)
        let cgroup_oom = self.cgroup.check_cgroup_oom();

        let details = match self.inspect() {
            Ok(details) => details,
            Err(err) => return (statistics, Err(err)),
        };
        let container_oom = self.check_container_oom(&details);

        // Return a different error if the container was killed due to an OOM
        let res = if container_oom || cgroup_oom {
            Err(match res {
                Ok(_) | Err(CommandError::ExecutionFailed { .. }) => CommandError::SandboxOOM,
                Err(err) => err,
            })
        } else {
            res
        };
        (statistics, res)
    }

    /// Run `docker rm -f` for this container, idempotently. On success the
    /// stored id is taken (so subsequent calls — including the one in
    /// [`Drop`] — are no-ops). On failure the id is restored so [`Drop`]
    /// (or a later call) can retry.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    fn delete(&mut self) -> Result<(), CommandError> {
        let Some(id) = self.id.take() else {
            return Ok(());
        };
        if let Err(err) = Command::new(self.workspace, "docker")
            .args(["rm", "-f", &id])
            .run()
        {
            self.id = Some(id);
            return Err(err);
        }
        Ok(())
    }
}

impl Drop for Container<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.delete() {
            error!(
                "docker rm failed, leaked sandbox container {}:\n{:?}",
                self.id.as_deref().unwrap_or_default(),
                err
            );
        }
    }
}

impl<'w> Sandbox<'w> {
    fn command_timed_out(res: &Result<ProcessOutput, CommandError>) -> bool {
        matches!(
            res,
            Err(CommandError::NoOutputFor(_))
                | Err(CommandError::Timeout(_))
                | Err(CommandError::KillAfterTimeoutFailed(_))
        )
    }

    /// Return the statistics gathered across the sandbox lifetime so far.
    pub fn statistics(&self) -> SandboxStatistics {
        self.statistics.snapshot()
    }

    #[cfg(test)]
    fn detect_host_cgroup(&mut self) -> Option<&HostCgroup> {
        self.container
            .as_mut()
            .and_then(|container| container.cgroup.detect_host_cgroup())
    }

    pub(crate) fn container_workdir(&self, path: &Path) -> Option<PathBuf> {
        let relative = path.strip_prefix(&self.source_dir).ok()?;
        Some(container_dirs::WORK_DIR.join(relative))
    }

    fn create_container(
        builder: &SandboxBuilder,
        workspace: &'w Workspace,
        source_dir: &Path,
        target_dir: &Path,
    ) -> Result<Container<'w>, CommandError> {
        builder
            .clone()
            .mount(
                source_dir,
                &container_dirs::WORK_DIR,
                builder.source_dir_mount_kind,
            )
            .mount(
                target_dir,
                &container_dirs::TARGET_DIR,
                MountKind::ReadWrite,
            )
            .mount(
                &workspace.cargo_home(),
                &container_dirs::CARGO_HOME,
                MountKind::ReadOnly,
            )
            .mount(
                &workspace.rustup_home(),
                &container_dirs::RUSTUP_HOME,
                MountKind::ReadOnly,
            )
            .create_started(workspace)
    }

    fn ensure_reusable_container(&mut self) -> Result<(), CommandError> {
        // The container can be stopped if a previous command got OOM-killed
        // at the container level, or missing after an explicit `cleanup()`.
        // Either way, recreate before attempting another `docker exec`.
        // Assigning the new value drops the old `Container`, whose `Drop`
        // impl runs `docker rm`.
        let needs_recreate = self
            .container
            .as_ref()
            .is_none_or(|container| !container.is_running());
        if needs_recreate {
            self.container = Some(Self::create_container(
                &self.builder,
                self.workspace,
                &self.source_dir,
                &self.target_dir,
            )?);
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            skip_all,
            fields(
                image = %self.workspace.sandbox_image().name,
                mounts = self.builder.mounts.len(),
                memory_limit = ?self.builder.memory_limit,
                cpu_limit = ?self.builder.cpu_limit,
                cpuset_cpus = ?self.builder.cpuset_cpus,
                enable_networking = self.builder.enable_networking,
                docker_runtime = ?self.builder.docker_runtime,
                capture,
                timeout_secs = ?timeout.map(|timeout| timeout.as_secs()),
                no_output_timeout_secs = ?no_output_timeout.map(|timeout| timeout.as_secs()),
            )
        )
    )]
    pub(crate) fn run(
        &mut self,
        command: SandboxCommand,
        timeout: Option<Duration>,
        no_output_timeout: Option<Duration>,
        process_lines: Option<&mut dyn FnMut(&str, &mut ProcessLinesActions)>,
        log_output: bool,
        log_command: bool,
        capture: bool,
    ) -> Result<ProcessOutput, CommandError> {
        let container_workdir = match command.workdir {
            Some(workdir) => self
                .container_workdir(&workdir)
                .expect("explicit workdir must be inside the sandbox source directory"),
            None => container_dirs::WORK_DIR.clone(),
        };
        let command = SandboxCommand {
            workdir: Some(container_workdir),
            ..command
        };
        self.ensure_reusable_container()?;
        let (statistics, res) = self.container.as_mut().unwrap().run_command(
            command,
            timeout,
            no_output_timeout,
            process_lines,
            log_output,
            log_command,
            capture,
        );
        self.statistics.merge(statistics);

        // On timeout we kill the host-side `docker exec` process, but the
        // command inside the container keeps running on the container's
        // `sleep infinity` init. Reusing the container would let the
        // abandoned process race the next command (sharing files, target
        // dir, CPU/memory budget). Tear the container down so the next
        // command in this build gets a clean one via
        // `ensure_reusable_container`.
        if Self::command_timed_out(&res)
            && let Some(mut container) = self.container.take()
        {
            container.delete()?;
        }

        res
    }

    /// Remove the live container owned by this sandbox and return the final
    /// statistics. Returns an error if `docker rm` fails; the container is
    /// also torn down by [`Drop`] as a fallback if this method is not called
    /// (or if its `docker rm` failed and a retry on drop succeeds).
    pub fn cleanup(&mut self) -> Result<SandboxStatistics, CommandError> {
        if let Some(container) = self.container.as_mut() {
            container.delete()?;
        }
        self.container = None;
        Ok(self.statistics())
    }
}

/// Check whether the Docker daemon is running.
///
/// The Docker daemon is required for sandboxing to work, and this function returns whether the
/// daemon is online and reachable or not. Calling a sandboxed command when the daemon is offline
/// will error too, but this function allows the caller to error earlier.
pub fn docker_running(workspace: &Workspace) -> bool {
    info!("checking if the docker daemon is running");
    Command::new(workspace, "docker")
        .args(["info"])
        .log_output(false)
        .run()
        .is_ok()
}

fn format_cpuset_cpus(cpus: &RangeInclusive<usize>) -> String {
    format!("{}-{}", cpus.start(), cpus.end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Workspace, WorkspaceBuilder, cmd::SandboxImage};
    use std::{env, path::Path};
    use tempfile::tempdir;
    use test_case::test_case;

    const USER_AGENT: &str = "rustwide-tests (https://github.com/rust-lang/rustwide)";

    fn sandbox_builder() -> SandboxBuilder {
        let builder = SandboxBuilder::new().enable_networking(false);
        let Ok(runtime) = env::var("RUSTWIDE_DOCKER_RUNTIME") else {
            return builder;
        };
        builder.docker_runtime(runtime.parse().expect("invalid RUSTWIDE_DOCKER_RUNTIME"))
    }

    #[test]
    fn formats_cpuset_cpus() {
        assert_eq!(format_cpuset_cpus(&(2..=4)), "2-4");
    }

    #[test_case("", Ok(DockerRuntime::Default))]
    #[test_case("default", Ok(DockerRuntime::Default))]
    #[test_case("runsc", Ok(DockerRuntime::Runsc))]
    #[test_case("runc", Err(ParseDockerRuntimeError))]
    fn parses_docker_runtime_values(
        value: &str,
        expected: Result<DockerRuntime, ParseDockerRuntimeError>,
    ) {
        assert_eq!(value.parse(), expected);
    }

    #[test_case(DockerRuntime::Default, None)]
    #[test_case(DockerRuntime::Runsc, Some("runsc"))]
    fn renders_docker_runtime_names(runtime: DockerRuntime, expected: Option<&str>) {
        assert_eq!(runtime.docker_name(), expected);
    }

    const fn stats(peak: Option<u64>) -> SandboxStatistics {
        SandboxStatistics { memory_peak: peak }
    }

    #[test_case(stats(None), stats(None), stats(None))]
    #[test_case(stats(Some(100)), stats(None), stats(Some(100)))]
    #[test_case(stats(None), stats(Some(100)), stats(Some(100)))]
    #[test_case(stats(Some(300)), stats(Some(100)), stats(Some(300)))]
    #[test_case(stats(Some(100)), stats(Some(300)), stats(Some(300)))]
    #[test_case(stats(Some(42)), stats(Some(42)), stats(Some(42)))]
    fn test_combine(lhs: SandboxStatistics, rhs: SandboxStatistics, expected: SandboxStatistics) {
        {
            let lhs = lhs.clone();
            let rhs = rhs.clone();
            assert_eq!(lhs.combine(rhs), expected);
        }

        {
            let mut lhs = lhs.clone();
            lhs.merge(rhs);
            assert_eq!(lhs, expected);
        }
    }

    #[test]
    fn merge_accumulate_over_multiple() {
        let mut s = stats(None);
        s.merge(stats(Some(50)));
        s.merge(stats(Some(200)));
        s.merge(stats(None));
        s.merge(stats(Some(150)));
        assert_eq!(s.memory_peak, Some(200));
    }

    fn init_test_workspace(name: &str) -> anyhow::Result<Workspace> {
        let workspace_path = Path::new(".workspaces").join(name);
        let mut builder = WorkspaceBuilder::new(&workspace_path, USER_AGENT).fast_init(true);

        if env::var("RUSTWIDE_TEST_INSIDE_DOCKER").is_ok() {
            builder = builder.running_inside_docker(true);
        }

        if cfg!(target_os = "linux") {
            builder = builder.sandbox_image(SandboxImage::remote(
                "ghcr.io/rust-lang/crates-build-env/linux-micro",
            )?);
        }

        builder.init()
    }

    #[test]
    #[cfg(not(windows))]
    fn detects_host_cgroup_files() -> anyhow::Result<()> {
        let workspace = init_test_workspace("build-unit")?;
        let source_dir = tempdir()?;
        let target_dir = tempdir()?;
        let mut sandbox =
            sandbox_builder().start(&workspace, source_dir.path(), target_dir.path())?;
        let host_cgroup = sandbox
            .detect_host_cgroup()
            .expect("sandbox should resolve host cgroup files");

        assert!(host_cgroup.memory_peak_file.is_file());
        assert!(host_cgroup.oom_kill_count_file.is_file());

        Ok(())
    }

    #[test]
    #[cfg(not(windows))]
    fn host_and_exec_memory_peaks_are_nonzero_and_close() -> anyhow::Result<()> {
        let workspace = init_test_workspace("build-unit")?;
        let source_dir = tempdir()?;
        let target_dir = tempdir()?;

        let builder = sandbox_builder();
        let supports_cgroup_files_inside_container = builder
            .docker_runtime
            .supports_cgroup_files_inside_container();

        let mut sandbox = builder.start(&workspace, source_dir.path(), target_dir.path())?;
        let host_cgroup = sandbox
            .detect_host_cgroup()
            .expect("sandbox should resolve host cgroup files");

        let host_peak = host_cgroup
            .read_memory_peak()
            .expect("host-side memory peak should be readable");

        assert!(host_peak > 0, "host-side memory peak should be nonzero");

        if !supports_cgroup_files_inside_container {
            return Ok(());
        }

        let exec_peak = sandbox
            .container
            .as_mut()
            .expect("sandbox container should be present")
            .cgroup
            .read_memory_peak_from_container()
            .expect("exec-side memory peak should be readable");

        assert!(exec_peak > 0, "exec-side memory peak should be nonzero");

        let min_peak = host_peak.min(exec_peak);
        let max_peak = host_peak.max(exec_peak);
        assert!(
            max_peak <= min_peak + 8 * 1024 * 1024,
            "host and exec peaks should be in the same ballpark: host={host_peak}, exec={exec_peak}",
        );

        Ok(())
    }

    #[test]
    #[cfg(not(windows))]
    fn host_and_exec_oom_kill_counts_match() -> anyhow::Result<()> {
        let workspace = init_test_workspace("build-unit")?;
        let source_dir = tempdir()?;
        let target_dir = tempdir()?;

        let builder = sandbox_builder();
        if !builder
            .docker_runtime
            .supports_cgroup_files_inside_container()
        {
            return Ok(());
        }

        let mut sandbox = builder.start(&workspace, source_dir.path(), target_dir.path())?;
        let host_cgroup = sandbox
            .detect_host_cgroup()
            .expect("sandbox should resolve host cgroup files");

        let host_oom_kill_count = host_cgroup
            .read_oom_kill_count()
            .expect("host-side oom_kill count should be readable");
        let exec_oom_kill_count = sandbox
            .container
            .as_mut()
            .expect("sandbox container should be present")
            .cgroup
            .read_oom_kill_count_from_container()
            .expect("exec-side oom_kill count should be readable");

        assert_eq!(host_oom_kill_count, exec_oom_kill_count);

        Ok(())
    }
}
