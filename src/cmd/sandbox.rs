use crate::cmd::{Command, CommandError, ProcessLinesActions, ProcessOutput};
use crate::Workspace;
use log::{error, info};
use serde::Deserialize;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Docker image used for sandboxing.
pub struct SandboxImage {
    name: String,
}

#[derive(serde::Deserialize)]
struct DockerManifest {
    config: DockerManifestConfig,
    layers: Vec<DockerManifestLayer>,
}

#[derive(serde::Deserialize)]
struct DockerManifestConfig {
    digest: String,
}

#[derive(serde::Deserialize)]
struct DockerManifestLayer {
    size: usize,
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
    pub fn remote(name: &str, size_limit: Option<usize>) -> Result<Self, CommandError> {
        let mut image = SandboxImage { name: name.into() };
        let digest = if let Some(size_limit) = size_limit {
            let out = Command::new_workspaceless("docker")
                .args(&["manifest", "inspect", name])
                .run_capture()?
                .stdout_lines()
                .join("\n");
            let m: DockerManifest = serde_json::from_str(&out)
                .map_err(CommandError::InvalidDockerManifestInspectOutput)?;
            let size = m.layers.iter().fold(0, |acc, l| acc + l.size);
            if size > size_limit {
                return Err(CommandError::SandboxImageTooLarge(size));
            }
            Some(m.config.digest)
        } else {
            None
        };
        info!("pulling image {} from Docker Hub", name);
        Command::new_workspaceless("docker")
            .args(&[
                "pull",
                &digest.map_or(name.to_string(), |digest| {
                    let name = name.split(':').next().unwrap();
                    format!("{}@{}", name, digest)
                }),
            ])
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

    fn get_name_with_hash(&self) -> Option<String> {
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
#[derive(Copy, Clone, PartialEq, Eq)]
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

/// The sandbox builder allows to configure a sandbox, used later in a
/// [`Command`](struct.Command.html).
#[derive(Clone)]
pub struct SandboxBuilder {
    mounts: Vec<MountConfig>,
    env: Vec<(String, String)>,
    memory_limit: Option<usize>,
    cpu_limit: Option<f32>,
    workdir: Option<String>,
    user: Option<String>,
    cmd: Vec<String>,
    enable_networking: bool,
    image: Option<String>,
}

impl SandboxBuilder {
    /// Create a new sandbox builder.
    pub fn new() -> Self {
        Self {
            mounts: Vec::new(),
            env: Vec::new(),
            workdir: None,
            memory_limit: None,
            cpu_limit: None,
            user: None,
            cmd: Vec::new(),
            enable_networking: true,
            image: None,
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

    /// Enable or disable the sandbox's networking. When it's disabled processes inside the sandbox
    /// won't be able to reach network service on the Internet or the host machine.
    ///
    /// By default networking is enabled.
    pub fn enable_networking(mut self, enable: bool) -> Self {
        self.enable_networking = enable;
        self
    }

    /// Override the image used for this sandbox.
    ///
    /// By default rustwide will use the image configured with [`WorkspaceBuilder::sandbox_image`].
    pub fn image(mut self, image: SandboxImage) -> Self {
        self.image = Some(image.name);
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
        self.user = Some(format!("{}:{}", user, group));
        self
    }

    fn create(self, workspace: &Workspace) -> Result<Container<'_>, CommandError> {
        let mut args: Vec<String> = vec!["create".into()];

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

        for &(ref var, ref value) in &self.env {
            args.push("-e".into());
            args.push(format! {"{}={}", var, value})
        }

        if let Some(workdir) = self.workdir {
            args.push("-w".into());
            args.push(workdir);
        }

        if let Some(limit) = self.memory_limit {
            args.push("-m".into());
            args.push(limit.to_string());
        }

        if let Some(limit) = self.cpu_limit {
            args.push("--cpus".into());
            args.push(limit.to_string());
        }

        if let Some(user) = self.user {
            args.push("--user".into());
            args.push(user);
        }

        if !self.enable_networking {
            args.push("--network".into());
            args.push("none".into());
        }

        if cfg!(windows) {
            args.push("--isolation=process".into());
        }

        if let Some(image) = self.image {
            args.push(image);
        } else {
            args.push(workspace.sandbox_image().name.clone());
        }

        for arg in self.cmd {
            args.push(arg);
        }

        let out = Command::new(workspace, "docker")
            .args(&*args)
            .run_capture()?;
        Ok(Container {
            id: out.stdout_lines()[0].clone(),
            workspace,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run(
        self,
        workspace: &Workspace,
        timeout: Option<Duration>,
        no_output_timeout: Option<Duration>,
        process_lines: Option<&mut dyn FnMut(&str, &mut ProcessLinesActions)>,
        log_output: bool,
        log_command: bool,
        capture: bool,
    ) -> Result<ProcessOutput, CommandError> {
        let container = self.create(workspace)?;

        // Ensure the container is properly deleted even if something panics
        scopeguard::defer! {{
            if let Err(err) = container.delete() {
                error!("failed to delete container {}", container.id);
                error!("caused by: {}", err);
                let mut err: &dyn Error = &err;
                while let Some(cause) = err.source() {
                    error!("caused by: {}", cause);
                    err = cause;
                }
            }
        }}

        container.run(
            timeout,
            no_output_timeout,
            process_lines,
            log_output,
            log_command,
            capture,
        )
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
}

#[derive(Clone)]
struct Container<'w> {
    // Docker container ID
    id: String,
    workspace: &'w Workspace,
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

    fn run(
        &self,
        timeout: Option<Duration>,
        no_output_timeout: Option<Duration>,
        process_lines: Option<&mut dyn FnMut(&str, &mut ProcessLinesActions)>,
        log_output: bool,
        log_command: bool,
        capture: bool,
    ) -> Result<ProcessOutput, CommandError> {
        let mut cmd = Command::new(self.workspace, "docker")
            .args(&["start", "-a", &self.id])
            .timeout(timeout)
            .log_output(log_output)
            .log_command(log_command)
            .no_output_timeout(no_output_timeout);

        if let Some(f) = process_lines {
            cmd = cmd.process_lines(f);
        }

        let res = cmd.run_inner(capture);
        let details = self.inspect()?;

        // Return a different error if the container was killed due to an OOM
        if details.state.oom_killed {
            Err(match res {
                Ok(_) | Err(CommandError::ExecutionFailed(_)) => CommandError::SandboxOOM,
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
