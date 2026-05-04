use crate::{
    Workspace,
    cmd::{Command, CommandError, ProcessLinesActions, ProcessOutput, container_dirs},
};
use log::{error, info};
use serde::Deserialize;
use std::{
    cell::Cell,
    error::Error,
    fmt,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    time::Duration,
};

/// The Docker image used for sandboxing.
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
            .args(&["pull", name])
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
            .args(&["image", "inspect", &self.name])
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
            .args(&[
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
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
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
    env: Vec<(String, String)>,
    source_dir_mount_kind: MountKind,
    memory_limit: Option<usize>,
    cpu_limit: Option<f32>,
    cpuset_cpus: Option<RangeInclusive<usize>>,
    workdir: Option<String>,
    user: Option<String>,
    cmd: Vec<String>,
    enable_networking: bool,
}

/// A live sandbox that can execute one or more commands.
///
/// Sandboxes created with [`SandboxBuilder::start`] are lazy: the container is
/// created only when a command is first run. The same sandbox can be reused
/// across multiple commands.
pub struct Sandbox<'w> {
    workspace: &'w Workspace,
    builder: SandboxBuilder,
    source_dir: PathBuf,
    target_dir: PathBuf,
    container: Option<Container<'w>>,
    memory_peak: Cell<Option<u64>>,
}

pub(crate) struct SandboxCommand<'a> {
    pub(crate) cmd: Vec<String>,
    pub(crate) env: &'a [(String, String)],
    pub(crate) workdir: Option<&'a str>,
    pub(crate) user: Option<&'a str>,
}

impl SandboxBuilder {
    /// Create a new sandbox builder.
    pub fn new() -> Self {
        Self {
            mounts: Vec::new(),
            env: Vec::new(),
            source_dir_mount_kind: MountKind::ReadOnly,
            workdir: None,
            memory_limit: None,
            cpu_limit: None,
            cpuset_cpus: None,
            user: None,
            cmd: Vec::new(),
            enable_networking: true,
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
    /// Be sure you understand the implications of setting this. If you set
    /// this to read-write, and the source directory may potentially be
    /// reused, then subsequent invocations may see those changes. Beware of
    /// trusting those previous invocations or the contents of the source
    /// directory.
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

    pub(super) fn env<S1: Into<String>, S2: Into<String>>(mut self, key: S1, value: S2) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub(super) fn cmd(mut self, cmd: Vec<String>) -> Self {
        self.cmd = cmd;
        self
    }

    pub(super) fn workdir<S: Into<String>>(mut self, workdir: S) -> Self {
        self.workdir = Some(workdir.into());
        self
    }

    pub(super) fn user(mut self, user: u32, group: u32) -> Self {
        self.user = Some(format!("{user}:{group}"));
        self
    }

    /// Start a live sandbox from this configuration.
    ///
    /// The returned sandbox can be used to run one or more commands against a
    /// fixed source directory and target directory. Container creation is
    /// deferred until the first command is executed.
    pub fn start<'w>(
        self,
        workspace: &'w Workspace,
        source_dir: PathBuf,
        target_dir: PathBuf,
    ) -> Sandbox<'w> {
        Sandbox {
            workspace,
            builder: self,
            source_dir: crate::utils::normalize_path(&source_dir),
            target_dir: crate::utils::normalize_path(&target_dir),
            container: None,
            memory_peak: Cell::new(None),
        }
    }

    fn create_started(self, workspace: &Workspace) -> Result<Container<'_>, CommandError> {
        let container = scopeguard::guard(self.create(workspace)?, |container| {
            container.delete_or_log();
        });
        container.start()?;
        container.record_oom_kill_count();
        Ok(scopeguard::ScopeGuard::into_inner(container))
    }

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

        args.push(workspace.sandbox_image().name.clone());

        // Use an idle command; the real command runs via `docker exec` so the container stays
        // alive after the command finishes, allowing us to read cgroup metrics.
        args.push("sleep".into());
        args.push("infinity".into());

        let out = Command::new(workspace, "docker")
            .args(&args)
            .run_capture()
            .map_err(|err| CommandError::SandboxContainerCreate(Box::new(err)))?;
        Ok(Container {
            id: out.stdout_lines()[0].clone(),
            workspace,
            running: Cell::new(true),
            oom_killed: Cell::new(false),
            oom_kill_count: Cell::new(None),
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
    #[serde(rename = "Running")]
    running: bool,
}

struct Container<'w> {
    // Docker container ID
    id: String,
    workspace: &'w Workspace,
    running: Cell<bool>,
    oom_killed: Cell<bool>,
    oom_kill_count: Cell<Option<u64>>,
}

impl fmt::Display for Container<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.id.fmt(f)
    }
}

impl Container<'_> {
    fn inspect(&self) -> Result<InspectContainer, CommandError> {
        let output = Command::new(self.workspace, "docker")
            .args(&["inspect", &self.id])
            .log_output(false)
            .run_capture()?;

        let mut data: Vec<InspectContainer> =
            ::serde_json::from_str(&output.stdout_lines().join("\n"))
                .map_err(CommandError::InvalidDockerInspectOutput)?;
        assert_eq!(data.len(), 1);
        Ok(data.pop().unwrap())
    }

    /// Start the container in detached mode (without `-a`).
    fn start(&self) -> Result<(), CommandError> {
        Command::new(self.workspace, "docker")
            .args(&["start", &self.id])
            .log_output(false)
            .run()
            .map(|_| ())
    }

    /// Helper to `docker exec cat <path>` and return stdout lines on success.
    fn exec_cat_file(&self, path: &str) -> Option<Vec<String>> {
        Command::new(self.workspace, "docker")
            .args(&["exec", &self.id, "cat", path])
            .log_output(false)
            .log_command(false)
            .run_capture()
            .ok()
            .map(|o| o.stdout_lines().to_vec())
    }

    fn record_oom_kill_count(&self) {
        self.oom_kill_count.set(self.read_oom_kill_count());
    }

    /// Best-effort read of peak memory usage from the still-running container.
    /// Tries cgroups v2 first, then falls back to cgroups v1.
    fn read_memory_peak(&self) -> Option<u64> {
        let paths = [
            "/sys/fs/cgroup/memory.peak",                      // v2
            "/sys/fs/cgroup/memory/memory.max_usage_in_bytes", // v1
        ];
        for path in paths {
            if let Some(val) = self
                .exec_cat_file(path)
                .and_then(|lines| lines.first()?.trim().parse::<u64>().ok())
            {
                return Some(val);
            }
        }
        None
    }

    /// Check if any OOM kills occurred in the container's cgroup.
    ///
    /// With the `docker exec` model, the OOM killer may only kill the exec'd process
    /// while `sleep infinity` (PID 1) survives. In that case `docker inspect` won't
    /// report `OOMKilled`, so we check the cgroup events directly.
    /// Tries cgroups v2 first, then falls back to cgroups v1.
    fn read_oom_kill_count(&self) -> Option<u64> {
        // Both v1 and v2 expose `oom_kill <count>` — just in different files.
        let paths = [
            "/sys/fs/cgroup/memory.events",             // v2
            "/sys/fs/cgroup/memory/memory.oom_control", // v1
        ];
        for path in paths {
            if let Some(lines) = self.exec_cat_file(path) {
                for line in &lines {
                    if let Some(count) = line
                        .strip_prefix("oom_kill ")
                        .and_then(|rest| rest.trim().parse::<u64>().ok())
                    {
                        return Some(count);
                    }
                }
                return Some(0);
            }
        }
        None
    }

    fn check_cgroup_oom(&self) -> bool {
        let current = self.read_oom_kill_count();
        let previous = self.oom_kill_count.replace(current);

        current.unwrap_or_default() > previous.unwrap_or_default()
    }

    fn check_container_oom(&self, details: &InspectContainer) -> bool {
        self.running.set(details.state.running);
        // `OOMKilled` can stay true after the first failure. Treat it as an
        // edge-triggered signal so later commands in the same container don't
        // keep being reported as fresh OOMs.
        let previous = self.oom_killed.replace(details.state.oom_killed);
        details.state.oom_killed && !previous
    }

    fn is_running(&self) -> bool {
        self.running.get()
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn run_command(
        &self,
        command: SandboxCommand<'_>,
        record_memory_peak: impl FnOnce(Option<u64>),
        timeout: Option<Duration>,
        no_output_timeout: Option<Duration>,
        process_lines: Option<&mut dyn FnMut(&str, &mut ProcessLinesActions)>,
        log_output: bool,
        log_command: bool,
        capture: bool,
    ) -> Result<ProcessOutput, CommandError> {
        // Build the `docker exec` command with env/workdir/user from the sandbox config
        let mut args: Vec<String> = vec!["exec".into()];

        for (var, value) in command.env {
            args.push("-e".into());
            args.push(format!("{var}={value}"));
        }

        if let Some(workdir) = command.workdir {
            args.push("-w".into());
            args.push(workdir.to_string());
        }

        if let Some(user) = command.user {
            args.push("--user".into());
            args.push(user.to_string());
        }

        args.push(self.id.clone());
        args.extend(command.cmd.iter().cloned());

        let mut cmd = Command::new(self.workspace, "docker")
            .args(&args)
            .timeout(timeout)
            .log_output(log_output)
            .log_command(log_command)
            .no_output_timeout(no_output_timeout);

        if let Some(f) = process_lines {
            cmd = cmd.process_lines(f);
        }

        let res = cmd.run_inner(capture);

        // Read peak memory usage while the container is still running (best-effort)
        let memory_peak = self.read_memory_peak();
        record_memory_peak(memory_peak);

        // Check OOM via cgroup events (catches cases where only the exec'd process
        // was killed, leaving the container's init process alive)
        let cgroup_oom = self.check_cgroup_oom();

        let details = self.inspect()?;
        let container_oom = self.check_container_oom(&details);

        // Return a different error if the container was killed due to an OOM
        if container_oom || cgroup_oom {
            Err(match res {
                Ok(_) | Err(CommandError::ExecutionFailed { .. }) => CommandError::SandboxOOM,
                Err(err) => err,
            })
        } else {
            res
        }
    }

    fn delete(&self) -> Result<(), CommandError> {
        Command::new(self.workspace, "docker")
            .args(&["rm", "-f", &self.id])
            .run()
            .map(|_| ())
    }

    fn delete_or_log(&self) {
        if let Err(err) = self.delete() {
            error!("failed to delete container {}", self.id);
            error!("caused by: {err}");
            let mut err: &dyn Error = &err;
            while let Some(cause) = err.source() {
                error!("caused by: {cause}");
                err = cause;
            }
        }
    }
}

impl<'w> Sandbox<'w> {
    fn update_memory_peak(peak: &Cell<Option<u64>>, memory_peak: Option<u64>) {
        let updated = match (peak.get(), memory_peak) {
            (Some(lhs), Some(rhs)) => Some(lhs.max(rhs)),
            (lhs, rhs) => lhs.or(rhs),
        };
        peak.set(updated);
    }

    pub(crate) fn memory_peak_bytes(&self) -> Option<u64> {
        self.memory_peak.get()
    }

    pub(crate) fn container_workdir(&self, path: &Path) -> Option<PathBuf> {
        let relative = path.strip_prefix(&self.source_dir).ok()?;
        Some(container_dirs::WORK_DIR.join(relative))
    }

    fn ensure_reusable_container(&mut self) -> Result<(), CommandError> {
        let mount_kind = self.builder.source_dir_mount_kind;

        // If a previous command killed the container itself, recreate it before
        // attempting another `docker exec`.
        if self
            .container
            .as_ref()
            .is_some_and(|container| !container.is_running())
            && let Some(container) = self.container.take()
        {
            container.delete()?;
        }

        if self.container.is_none() {
            let container = self
                .builder
                .clone()
                .mount(&self.source_dir, &container_dirs::WORK_DIR, mount_kind)
                .mount(
                    &self.target_dir,
                    &container_dirs::TARGET_DIR,
                    MountKind::ReadWrite,
                )
                .mount(
                    &self.workspace.cargo_home(),
                    &container_dirs::CARGO_HOME,
                    MountKind::ReadOnly,
                )
                .mount(
                    &self.workspace.rustup_home(),
                    &container_dirs::RUSTUP_HOME,
                    MountKind::ReadOnly,
                )
                .create_started(self.workspace)?;
            self.container = Some(container);
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    pub(crate) fn run(
        &mut self,
        command: SandboxCommand<'_>,
        timeout: Option<Duration>,
        no_output_timeout: Option<Duration>,
        process_lines: Option<&mut dyn FnMut(&str, &mut ProcessLinesActions)>,
        log_output: bool,
        log_command: bool,
        capture: bool,
    ) -> Result<ProcessOutput, CommandError> {
        if let Some(container_workdir) = command
            .workdir
            .and_then(|workdir| self.container_workdir(Path::new(workdir)))
        {
            let command = SandboxCommand {
                workdir: Some(container_workdir.to_str().unwrap()),
                ..command
            };
            self.ensure_reusable_container()?;
            let container = self.container.as_ref().unwrap();
            let peak = &self.memory_peak;
            return container.run_command(
                command,
                |memory_peak| Self::update_memory_peak(peak, memory_peak),
                timeout,
                no_output_timeout,
                process_lines,
                log_output,
                log_command,
                capture,
            );
        }

        let workdir = command.workdir.unwrap_or(".");
        let mount_kind = self.builder.source_dir_mount_kind;
        let mut ephemeral = self
            .builder
            .clone()
            .mount(Path::new(workdir), &container_dirs::WORK_DIR, mount_kind)
            .mount(
                &self.workspace.cargo_home(),
                &container_dirs::CARGO_HOME,
                MountKind::ReadOnly,
            )
            .mount(
                &self.workspace.rustup_home(),
                &container_dirs::RUSTUP_HOME,
                MountKind::ReadOnly,
            )
            .env("SOURCE_DIR", container_dirs::WORK_DIR.to_str().unwrap())
            .env("CARGO_HOME", container_dirs::CARGO_HOME.to_str().unwrap())
            .env("RUSTUP_HOME", container_dirs::RUSTUP_HOME.to_str().unwrap())
            .workdir(container_dirs::WORK_DIR.to_str().unwrap())
            .cmd(command.cmd.clone())
            .mount(
                &self.target_dir,
                &container_dirs::TARGET_DIR,
                MountKind::ReadWrite,
            );
        for (key, value) in command.env {
            if key != "SOURCE_DIR" && key != "CARGO_HOME" && key != "RUSTUP_HOME" {
                ephemeral = ephemeral.env(key, value);
            }
        }
        if let Some(user) = command.user {
            let (uid, gid) = user
                .split_once(':')
                .and_then(|(uid, gid)| Some((uid.parse::<u32>().ok()?, gid.parse::<u32>().ok()?)))
                .expect("invalid user format");
            ephemeral = ephemeral.user(uid, gid);
        }

        let env = ephemeral.env.clone();
        let workdir = ephemeral.workdir.clone();
        let user = ephemeral.user.clone();
        let command = SandboxCommand {
            cmd: ephemeral.cmd.clone(),
            env: &env,
            workdir: workdir.as_deref(),
            user: user.as_deref(),
        };
        let container = ephemeral.create_started(self.workspace)?;

        scopeguard::defer! {{
            container.delete_or_log();
        }}

        let peak = &self.memory_peak;
        container.run_command(
            command,
            |memory_peak| Self::update_memory_peak(peak, memory_peak),
            timeout,
            no_output_timeout,
            process_lines,
            log_output,
            log_command,
            capture,
        )
    }

    pub(crate) fn cleanup(&mut self) -> Result<(), CommandError> {
        if let Some(container) = self.container.take() {
            container.delete()?;
        }

        Ok(())
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
        .args(&["info"])
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

    #[test]
    fn formats_cpuset_cpus() {
        assert_eq!(format_cpuset_cpus(&(2..=4)), "2-4");
    }
}
