use crate::cmd::Command;
use crate::workspace::Workspace;
use failure::Error;
use getrandom::getrandom;
use log::info;

static PROBE_FILENAME: &str = "rustwide-probe";

pub(crate) struct CurrentContainer {
    metadata: Metadata,
}

impl CurrentContainer {
    pub(crate) fn detect(workspace: &Workspace) -> Result<Option<Self>, Error> {
        if let Some(id) = probe_container_id(workspace)? {
            info!("inspecting the current container");
            let inspect = Command::new(workspace, "docker")
                .args(&["inspect", &id])
                .log_output(false)
                .log_command(false)
                .run_capture()?;
            let content = inspect.stdout_lines().join("\n");
            let mut metadata: Vec<Metadata> = serde_json::from_str(&content)?;
            if metadata.len() != 1 {
                failure::bail!("invalid output returned by `docker inspect`");
            }
            Ok(Some(CurrentContainer {
                metadata: metadata.pop().unwrap(),
            }))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn mounts(&self) -> &[Mount] {
        &self.metadata.mounts
    }
}

/// Apparently there is no cross platform way to easily get the current container ID from Docker
/// itself. On Linux is possible to inspect the cgroups and parse the ID out of there, but of
/// course cgroups are not available on Windows.
///
/// This function uses a simpler but slower method to get the ID: a file with a random string is
/// created in the temp directory, the list of all the containers is fetched from Docker and then
/// `cat` is executed inside each of them to check whether they have the same random string.
pub(crate) fn probe_container_id(workspace: &Workspace) -> Result<Option<String>, Error> {
    info!("detecting the ID of the container where rustwide is running");

    // Create the probe on the current file system
    let probe_path = std::env::temp_dir().join(PROBE_FILENAME);
    let probe_path_str = probe_path.to_str().unwrap();
    let mut probe_content = [0u8; 64];
    getrandom(&mut probe_content)?;
    let probe_content = base64::encode(&probe_content[..]);
    std::fs::write(&probe_path, probe_content.as_bytes())?;

    // Check if the probe exists on any of the currently running containers.
    let out = Command::new(workspace, "docker")
        .args(&["ps", "--format", "{{.ID}}", "--no-trunc"])
        .log_output(false)
        .log_command(false)
        .run_capture()?;
    for id in out.stdout_lines() {
        info!("probing container id {}", id);

        let res = Command::new(workspace, "docker")
            .args(&["exec", id, "cat", probe_path_str])
            .log_output(false)
            .log_command(false)
            .run_capture();
        if let Ok([probed]) = res.as_ref().map(|out| out.stdout_lines()) {
            if *probed == probe_content {
                info!("probe successful, this is container ID {}", id);
                return Ok(Some(id.clone()));
            }
        }
    }

    info!("probe unsuccessful, this is not running inside a container");
    Ok(None)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Metadata {
    mounts: Vec<Mount>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Mount {
    source: String,
    destination: String,
}

impl Mount {
    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn destination(&self) -> &str {
        &self.destination
    }
}
