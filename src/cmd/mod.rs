//! Command execution and sandboxing.

mod process_lines_actions;
mod sandbox;

pub use process_lines_actions::ProcessLinesActions;
pub use sandbox::*;

use crate::native;
use crate::workspace::Workspace;
use futures_util::{
    future::{self, FutureExt},
    stream::{self, TryStreamExt},
};
use log::{error, info};
use process_lines_actions::InnerState;
use std::convert::AsRef;
use std::env::consts::EXE_SUFFIX;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command as AsyncCommand,
    runtime::Runtime,
    time,
};
use tokio_stream::{wrappers::LinesStream, StreamExt};

lazy_static::lazy_static! {
    // TODO: Migrate to asynchronous code and remove runtime
    pub(super) static ref RUNTIME: Runtime = Runtime::new().expect("Failed to construct tokio runtime");
}

pub(crate) mod container_dirs {
    use lazy_static::lazy_static;
    use std::path::{Path, PathBuf};

    #[cfg(windows)]
    lazy_static! {
        pub(super) static ref ROOT_DIR: PathBuf = Path::new(r"C:\rustwide").into();
    }

    #[cfg(not(windows))]
    lazy_static! {
        pub(super) static ref ROOT_DIR: PathBuf = Path::new("/opt/rustwide").into();
    }

    lazy_static! {
        pub(crate) static ref WORK_DIR: PathBuf = ROOT_DIR.join("workdir");
        pub(crate) static ref TARGET_DIR: PathBuf = ROOT_DIR.join("target");
        pub(super) static ref CARGO_HOME: PathBuf = ROOT_DIR.join("cargo-home");
        pub(super) static ref RUSTUP_HOME: PathBuf = ROOT_DIR.join("rustup-home");
        pub(super) static ref CARGO_BIN_DIR: PathBuf = CARGO_HOME.join("bin");
    }
}

/// Error happened while executing a command.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommandError {
    /// The command didn't output anything to stdout or stderr for more than the timeout, and it
    /// was killed. The timeout's value (in seconds) is the first value.
    #[error("no output for {0} seconds")]
    NoOutputFor(u64),

    /// The command took more time than the timeout to end, and it was killed. The timeout's value
    /// (in seconds) is the first value.
    #[error("command timed out after {0} seconds")]
    Timeout(u64),

    /// The command failed to execute.
    #[error("command failed: {status}\n\n{stderr}")]
    ExecutionFailed {
        /// the exit status we got from the command
        status: ExitStatus,
        /// the stderr output, if it was captured via `.run_capture()`
        stderr: String,
    },

    /// Killing the underlying process after the timeout failed.
    #[error("{0}")]
    KillAfterTimeoutFailed(#[source] KillFailedError),

    /// The sandbox ran out of memory and was killed.
    #[error("container ran out of memory")]
    SandboxOOM,

    /// Pulling a sandbox image from the registry failed
    #[error("failed to pull the sandbox image from the registry: {0}")]
    SandboxImagePullFailed(#[source] Box<CommandError>),

    /// The sandbox image is missing from the local system.
    #[error("sandbox image missing from the local system: {0}")]
    SandboxImageMissing(#[source] Box<CommandError>),

    /// Failed to create the sandbox container
    #[error("sandbox container could not be created: {0}")]
    SandboxContainerCreate(#[source] Box<CommandError>),

    /// Running rustwide inside a Docker container requires the workspace directory to be mounted
    /// from the host system. This error happens if that's not true, for example if the workspace
    /// lives in a directory inside the container.
    #[error("the workspace is not mounted from outside the container")]
    WorkspaceNotMountedCorrectly,

    /// The data received from the `docker inspect` command is not valid.
    #[error("invalid output of `docker inspect`: {0}")]
    InvalidDockerInspectOutput(#[source] serde_json::Error),

    /// An I/O error occured while executing the command.
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

/// Error happened while trying to kill a process.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(unix, error(
    "failed to kill the process with PID {pid}{}",
    .errno.map(|e| format!(": {}", e.desc())).unwrap_or_default()
))]
#[cfg_attr(not(unix), error("failed to kill the process with PID {pid}"))]
pub struct KillFailedError {
    pub(crate) pid: u32,
    #[cfg(unix)]
    pub(crate) errno: Option<nix::errno::Errno>,
}

impl KillFailedError {
    /// Return the PID of the process that couldn't be killed.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Return the underlying error number provided by the operative system.
    #[cfg(any(unix, doc))]
    #[cfg_attr(docs_rs, doc(cfg(unix)))]
    pub fn errno(&self) -> Option<i32> {
        self.errno.map(|errno| errno as i32)
    }
}

/// Name and kind of a binary executed by [`Command`](struct.Command.html).
#[non_exhaustive]
pub enum Binary {
    /// Global binary, available in `$PATH`. Rustwide doesn't apply any tweaks to its execution
    /// environment.
    Global(PathBuf),
    /// Binary installed and managed by Rustwide in its local rustup installation. Rustwide will
    /// tweak the environment to use the local rustup instead of the host system one, and will
    /// search the binary in the cargo home.
    ManagedByRustwide(PathBuf),
}

/// Trait representing a command that can be run by [`Command`](struct.Command.html).
pub trait Runnable {
    /// The name of the binary to execute.
    fn name(&self) -> Binary;

    /// Prepare the command for execution. This method is called as soon as a
    /// [`Command`](struct.Command.html) instance is created, and allows tweaking the command to
    /// better suit your binary, for example by adding default arguments or environment variables.
    ///
    /// The default implementation simply returns the provided command without changing anything in
    /// it.
    fn prepare_command<'w, 'pl>(&self, cmd: Command<'w, 'pl>) -> Command<'w, 'pl> {
        cmd
    }
}

impl Runnable for &str {
    fn name(&self) -> Binary {
        Binary::Global(self.into())
    }
}

impl Runnable for String {
    fn name(&self) -> Binary {
        Binary::Global(self.into())
    }
}

impl<B: Runnable> Runnable for &B {
    fn name(&self) -> Binary {
        Runnable::name(*self)
    }

    fn prepare_command<'w, 'pl>(&self, cmd: Command<'w, 'pl>) -> Command<'w, 'pl> {
        Runnable::prepare_command(*self, cmd)
    }
}

/// The `Command` is a builder to execute system commands and interact with them.
///
/// It's a more advanced version of [`std::process::Command`][std], featuring timeouts, realtime
/// output processing, output logging and sandboxing.
///
/// [std]: https://doc.rust-lang.org/std/process/struct.Command.html
#[must_use = "call `.run()` to run the command"]
#[allow(clippy::type_complexity)]
pub struct Command<'w, 'pl> {
    workspace: Option<&'w Workspace>,
    sandbox: Option<SandboxBuilder>,
    binary: Binary,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    process_lines: Option<&'pl mut dyn FnMut(&str, &mut ProcessLinesActions)>,
    cd: Option<PathBuf>,
    timeout: Option<Duration>,
    no_output_timeout: Option<Duration>,
    log_command: bool,
    log_output: bool,
    cargo_home_mount_kind: MountKind,
}

impl<'w, 'pl> Command<'w, 'pl> {
    /// Create a new, unsandboxed command.
    pub fn new<R: Runnable>(workspace: &'w Workspace, binary: R) -> Self {
        binary.prepare_command(Self::new_inner(binary.name(), Some(workspace), None))
    }

    /// Create a new, sandboxed command.
    pub fn new_sandboxed<R: Runnable>(
        workspace: &'w Workspace,
        sandbox: SandboxBuilder,
        binary: R,
    ) -> Self {
        binary.prepare_command(Self::new_inner(
            binary.name(),
            Some(workspace),
            Some(sandbox),
        ))
    }

    pub(crate) fn new_workspaceless<R: Runnable>(binary: R) -> Self {
        binary.prepare_command(Self::new_inner(binary.name(), None, None))
    }

    fn new_inner(
        binary: Binary,
        workspace: Option<&'w Workspace>,
        sandbox: Option<SandboxBuilder>,
    ) -> Self {
        let (timeout, no_output_timeout) = if let Some(workspace) = workspace {
            (
                workspace.default_command_timeout(),
                workspace.default_command_no_output_timeout(),
            )
        } else {
            (None, None)
        };
        Command {
            workspace,
            sandbox,
            binary,
            args: Vec::new(),
            env: Vec::new(),
            process_lines: None,
            cd: None,
            timeout,
            no_output_timeout,
            log_output: true,
            log_command: true,
            cargo_home_mount_kind: MountKind::ReadOnly,
        }
    }

    /// Mount the cargo home directory as read-write in the sandbox.
    pub fn rw_cargo_home(mut self) -> Self {
        self.cargo_home_mount_kind = MountKind::ReadWrite;
        self
    }

    /// Add command-line arguments to the command. This method can be called multiple times to add
    /// additional args.
    pub fn args<S: AsRef<OsStr>>(mut self, args: &[S]) -> Self {
        for arg in args {
            self.args.push(arg.as_ref().to_os_string());
        }

        self
    }

    /// Add an environment variable to the command.
    pub fn env<S1: AsRef<OsStr>, S2: AsRef<OsStr>>(mut self, key: S1, value: S2) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// Change the directory where the command will be executed in.
    pub fn cd<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.cd = Some(path.as_ref().to_path_buf());
        self
    }

    /// Set the timeout of this command. If it runs for more time the process will be killed.
    ///
    /// Its default value is configured through
    /// [`WorkspaceBuilder::command_timeout`](../struct.WorkspaceBuilder.html#method.command_timeout).
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the no output timeout of this command. If it doesn't output anything for more time the
    /// process will be killed.
    ///
    /// Its default value is configured through
    /// [`WorkspaceBuilder::command_no_output_timeout`](../struct.WorkspaceBuilder.html#method.command_no_output_timeout).
    pub fn no_output_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.no_output_timeout = timeout;
        self
    }

    /// Set the function that will be called each time a line is outputted to either the standard
    /// output or the standard error. Only one function can be set at any time for a command.
    ///
    /// The method is useful to analyze the command's output without storing all of it in memory.
    /// This example builds a crate and detects compiler errors (ICEs):
    ///
    /// ```no_run
    /// # use rustwide::{cmd::Command, WorkspaceBuilder};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// let mut ice = false;
    /// Command::new(&workspace, "cargo")
    ///     .args(&["build", "--all"])
    ///     .process_lines(&mut |line, _| {
    ///         if line.contains("internal compiler error") {
    ///             ice = true;
    ///         }
    ///     })
    ///     .run()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn process_lines(mut self, f: &'pl mut dyn FnMut(&str, &mut ProcessLinesActions)) -> Self {
        self.process_lines = Some(f);
        self
    }

    /// Enable or disable logging all the output lines to the [`log` crate][log]. By default
    /// logging is enabled.
    ///
    /// [log]: https://crates.io/crates/log
    pub fn log_output(mut self, log_output: bool) -> Self {
        self.log_output = log_output;
        self
    }

    /// Enable or disable logging the command name and args to the [`log` crate][log] before the
    /// exectuion. By default logging is enabled.
    ///
    /// [log]: https://crates.io/crates/log
    pub fn log_command(mut self, log_command: bool) -> Self {
        self.log_command = log_command;
        self
    }

    /// Run the prepared command and return an error if it fails (for example with a non-zero exit
    /// code or a timeout).
    pub fn run(self) -> Result<(), CommandError> {
        self.run_inner(false)?;
        Ok(())
    }

    /// Run the prepared command and return its output if it succeedes. If it fails (for example
    /// with a non-zero exit code or a timeout) an error will be returned instead.
    ///
    /// Even though the output will be captured and returned, if output logging is enabled (as it
    /// is by default) the output will be also logged. You can disable this behavior by calling the
    /// [`log_output`](struct.Command.html#method.log_output) method.
    pub fn run_capture(self) -> Result<ProcessOutput, CommandError> {
        self.run_inner(true)
    }

    fn run_inner(self, capture: bool) -> Result<ProcessOutput, CommandError> {
        if let Some(mut builder) = self.sandbox {
            let workspace = self
                .workspace
                .expect("sandboxed builds without a workspace are not supported");
            let binary = match self.binary {
                Binary::Global(path) => path,
                Binary::ManagedByRustwide(path) => {
                    container_dirs::CARGO_BIN_DIR.join(exe_suffix(path.as_os_str()))
                }
            };

            let mut cmd = vec![binary.to_string_lossy().as_ref().to_string()];

            for arg in self.args {
                cmd.push(arg.to_string_lossy().to_string());
            }

            let source_dir = match self.cd {
                Some(path) => path,
                None => PathBuf::from("."),
            };

            builder = builder
                .mount(&source_dir, &container_dirs::WORK_DIR, MountKind::ReadOnly)
                .env("SOURCE_DIR", container_dirs::WORK_DIR.to_str().unwrap())
                .workdir(container_dirs::WORK_DIR.to_str().unwrap())
                .cmd(cmd);

            if let Some(user) = native::current_user() {
                builder = builder.user(user.user_id, user.group_id);
            }

            for (key, value) in self.env {
                builder = builder.env(
                    key.to_string_lossy().as_ref(),
                    value.to_string_lossy().as_ref(),
                );
            }

            builder = builder
                .mount(
                    &workspace.cargo_home(),
                    &container_dirs::CARGO_HOME,
                    self.cargo_home_mount_kind,
                )
                .mount(
                    &workspace.rustup_home(),
                    &container_dirs::RUSTUP_HOME,
                    MountKind::ReadOnly,
                )
                .env("CARGO_HOME", container_dirs::CARGO_HOME.to_str().unwrap())
                .env("RUSTUP_HOME", container_dirs::RUSTUP_HOME.to_str().unwrap());

            builder.run(
                workspace,
                self.timeout,
                self.no_output_timeout,
                self.process_lines,
                self.log_output,
                self.log_command,
                capture,
            )
        } else {
            let (binary, managed_by_rustwide) = match self.binary {
                // global paths should never be normalized
                Binary::Global(path) => (path, false),
                Binary::ManagedByRustwide(path) => {
                    // `cargo_home()` might a relative path
                    let cargo_home = crate::utils::normalize_path(
                        &self
                            .workspace
                            .expect("calling rustwide bins without a workspace is not supported")
                            .cargo_home(),
                    );
                    let binary = cargo_home.join("bin").join(exe_suffix(path.as_os_str()));
                    (binary, true)
                }
            };

            let mut cmd = AsyncCommand::new(binary);
            cmd.args(&self.args);

            if managed_by_rustwide {
                let workspace = self
                    .workspace
                    .expect("calling rustwide bins without a workspace is not supported");
                let cargo_home = workspace
                    .cargo_home()
                    .to_str()
                    .expect("bad cargo home")
                    .to_string();
                let rustup_home = workspace
                    .rustup_home()
                    .to_str()
                    .expect("bad rustup home")
                    .to_string();
                cmd.env(
                    "CARGO_HOME",
                    crate::utils::normalize_path(cargo_home.as_ref()),
                );
                cmd.env(
                    "RUSTUP_HOME",
                    crate::utils::normalize_path(rustup_home.as_ref()),
                );
            }
            for (k, v) in &self.env {
                cmd.env(k, v);
            }

            let cmdstr = format!("{:?}", cmd);

            if let Some(ref cd) = self.cd {
                cmd.current_dir(cd);
            }

            if self.log_command {
                info!("running `{}`", cmdstr);
            }

            let out = RUNTIME
                .block_on(log_command(
                    cmd,
                    self.process_lines,
                    capture,
                    self.timeout,
                    self.no_output_timeout,
                    self.log_output,
                ))
                .map_err(|e| {
                    error!("error running command: {}", e);
                    e
                })?;

            if out.status.success() {
                Ok(out.into())
            } else {
                Err(CommandError::ExecutionFailed {
                    status: out.status,
                    stderr: out.stderr.join("\n"),
                })
            }
        }
    }
}

struct InnerProcessOutput {
    status: ExitStatus,
    stdout: Vec<String>,
    stderr: Vec<String>,
}

impl From<InnerProcessOutput> for ProcessOutput {
    fn from(orig: InnerProcessOutput) -> ProcessOutput {
        ProcessOutput {
            stdout: orig.stdout,
            stderr: orig.stderr,
        }
    }
}

/// Output of a [`Command`](struct.Command.html) when it was executed with the
/// [`run_capture`](struct.Command.html#method.run_capture) method.
pub struct ProcessOutput {
    stdout: Vec<String>,
    stderr: Vec<String>,
}

impl ProcessOutput {
    /// Return a list of the lines printed by the process on the standard output.
    pub fn stdout_lines(&self) -> &[String] {
        &self.stdout
    }

    /// Return a list of the lines printed by the process on the standard error.
    pub fn stderr_lines(&self) -> &[String] {
        &self.stderr
    }
}

enum OutputKind {
    Stdout,
    Stderr,
}

impl OutputKind {
    fn prefix(&self) -> &'static str {
        match *self {
            OutputKind::Stdout => "stdout",
            OutputKind::Stderr => "stderr",
        }
    }
}

#[allow(clippy::type_complexity)]
async fn log_command(
    mut cmd: AsyncCommand,
    mut process_lines: Option<&mut dyn FnMut(&str, &mut ProcessLinesActions)>,
    capture: bool,
    timeout: Option<Duration>,
    no_output_timeout: Option<Duration>,
    log_output: bool,
) -> Result<InnerProcessOutput, CommandError> {
    let timeout = if let Some(t) = timeout {
        t
    } else {
        // If timeouts are disabled just use a *really* long timeout
        // FIXME: this hack is horrible
        Duration::from_secs(7 * 24 * 60 * 60)
    };
    let no_output_timeout = if let Some(t) = no_output_timeout {
        t
    } else {
        // If the no output timeout is disabled set it the same as the full timeout.
        timeout
    };

    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let child_id = child.id().unwrap();

    let stdout = LinesStream::new(BufReader::new(child.stdout.take().unwrap()).lines())
        .map(|line| (OutputKind::Stdout, line));
    let stderr = LinesStream::new(BufReader::new(child.stderr.take().unwrap()).lines())
        .map(|line| (OutputKind::Stderr, line));

    let start = Instant::now();
    let mut actions = ProcessLinesActions::new();

    let output = stream::select(stdout, stderr)
        .timeout(no_output_timeout)
        .map(move |result| match result {
            // If the timeout elapses, kill the process
            Err(_timeout) => Err(match native::kill_process(child_id) {
                Ok(()) => CommandError::NoOutputFor(no_output_timeout.as_secs()),
                Err(err) => CommandError::KillAfterTimeoutFailed(err),
            }),

            // If an error occurred reading the line, flatten the error
            Ok((_, Err(read_err))) => Err(read_err.into()),

            // If the read was successful, return the `OutputKind` and the read line
            Ok((out_kind, Ok(line))) => Ok((out_kind, line)),
        })
        .and_then(move |(kind, line): (OutputKind, String)| {
            // If the process is in a tight output loop the timeout on the process might fail to
            // be executed, so this extra check prevents the process to run without limits.
            if start.elapsed() > timeout {
                return future::err(CommandError::Timeout(timeout.as_secs()));
            }

            if let Some(f) = &mut process_lines {
                f(&line, &mut actions);
            }
            // this is done here to avoid duplicating the output line
            let lines = match actions.take_lines() {
                InnerState::Removed => Vec::new(),
                InnerState::Original => vec![line],
                InnerState::Replaced(new_lines) => new_lines,
            };

            if log_output {
                for line in &lines {
                    info!("[{}] {}", kind.prefix(), line);
                }
            }

            future::ok((kind, lines))
        })
        .try_fold(
            (Vec::<String>::new(), Vec::<String>::new()),
            move |(mut stdout, mut stderr), (kind, mut lines)| async move {
                // If stdio/stdout is supposed to be captured, append it to
                // the accumulated stdio/stdout
                if capture {
                    match kind {
                        OutputKind::Stdout => stdout.append(&mut lines),
                        OutputKind::Stderr => stderr.append(&mut lines),
                    }
                }

                Ok((stdout, stderr))
            },
        );

    let child = time::timeout(timeout, child.wait()).map(move |result| {
        match result {
            // If the timeout elapses, kill the process
            Err(_timeout) => Err(match native::kill_process(child_id) {
                Ok(()) => CommandError::Timeout(timeout.as_secs()),
                Err(err) => CommandError::KillAfterTimeoutFailed(err),
            }),

            // If an error occurred with the child
            Ok(Err(err)) => Err(err.into()),

            // If the read was successful, return the process's exit status
            Ok(Ok(exit_status)) => Ok(exit_status),
        }
    });

    let ((stdout, stderr), status) = {
        let (output, child) = future::join(output, child).await;
        let (stdout, stderr) = output?;

        ((stdout, stderr), child?)
    };

    Ok(InnerProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn exe_suffix(file: &OsStr) -> OsString {
    let mut path = OsString::from(file);
    path.push(EXE_SUFFIX);
    path
}
