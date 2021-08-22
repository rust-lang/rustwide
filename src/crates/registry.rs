use super::CrateTrait;
use crate::Workspace;
use failure::{Error, ResultExt};
use flate2::read::GzDecoder;
use log::info;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use tar::Archive;

static CRATES_ROOT: &str = "https://static.crates.io/crates";

pub(crate) struct AlternativeRegistry {
    registry_index: String,
}

impl AlternativeRegistry {
    pub(crate) fn new(registry_index: impl Into<String>) -> AlternativeRegistry {
        AlternativeRegistry {
            registry_index: registry_index.into(),
        }
    }

    fn index(&self) -> &str {
        self.registry_index.as_str()
    }

    fn index_folder(&self) -> String {
        crate::utils::escape_path(self.registry_index.as_bytes())
    }
}

pub(crate) enum Registry {
    CratesIo,
    Alternative(AlternativeRegistry),
}

impl Registry {
    fn cache_folder(&self) -> String {
        match self {
            Registry::CratesIo => "cratesio-sources".into(),
            Registry::Alternative(alt) => format!("{}-sources", alt.index_folder()),
        }
    }

    fn name(&self) -> String {
        match self {
            Registry::CratesIo => "crates.io".into(),
            Registry::Alternative(alt) => alt.index().to_string(),
        }
    }
}

pub(super) struct RegistryCrate {
    registry: Registry,
    name: String,
    version: String,
    key: Option<String>,
}

#[derive(serde::Deserialize)]
struct IndexConfig {
    dl: String,
}

impl RegistryCrate {
    pub(super) fn new(registry: Registry, name: &str, version: &str, key: Option<String>) -> Self {
        RegistryCrate {
            registry,
            name: name.into(),
            version: version.into(),
            key: key.map(Into::into),
        }
    }

    fn cache_path(&self, workspace: &Workspace) -> PathBuf {
        workspace
            .cache_dir()
            .join(self.registry.cache_folder())
            .join(&self.name)
            .join(format!("{}-{}.crate", self.name, self.version))
    }

    fn fetch_url(&self, workspace: &Workspace) -> Result<String, Error> {
        match &self.registry {
            Registry::CratesIo => Ok(format!(
                "{0}/{1}/{1}-{2}.crate",
                CRATES_ROOT, self.name, self.version
            )),
            Registry::Alternative(alt) => {
                let index_path = workspace
                    .cache_dir()
                    .join("registry-index")
                    .join(alt.index_folder());
                if !index_path.exists() {
                    let url = alt.index();
                    let mut fo = git2::FetchOptions::new();
                    if let Some(key) = self.key.as_deref() {
                        fo.remote_callbacks({
                            let mut callbacks = git2::RemoteCallbacks::new();
                            callbacks.credentials(
                                move |_url, username_from_url, _allowed_types| {
                                    git2::Cred::ssh_key_from_memory(
                                        username_from_url.unwrap(),
                                        None,
                                        key,
                                        None,
                                    )
                                },
                            );
                            callbacks
                        });
                    }

                    git2::build::RepoBuilder::new()
                        .fetch_options(fo)
                        .clone(url, &index_path)
                        .with_context(|_| format!("unable to update_index at {}", url))?;
                    info!("cloned registry index");
                }
                let config = std::fs::read_to_string(index_path.join("config.json"))?;
                let template_url = serde_json::from_str::<IndexConfig>(&config)
                    .context("registry has invalid config.json")?
                    .dl;
                let replacements = [("{crate}", &self.name), ("{version}", &self.version)];

                let url = if replacements
                    .iter()
                    .any(|(key, _)| template_url.contains(key))
                {
                    let mut url = template_url;
                    for (key, value) in &replacements {
                        url = url.replace(key, value);
                    }
                    url
                } else {
                    format!("{}/{}/{}/download", template_url, self.name, self.version)
                };

                Ok(url)
            }
        }
    }
}

impl CrateTrait for RegistryCrate {
    fn fetch(&self, workspace: &Workspace) -> Result<(), Error> {
        let local = self.cache_path(workspace);
        if local.exists() {
            info!("crate {} {} is already in cache", self.name, self.version);
            return Ok(());
        }

        info!("fetching crate {} {}...", self.name, self.version);
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent)?;
        }

        workspace
            .http_client()
            .get(&self.fetch_url(workspace)?)
            .send()?
            .error_for_status()?
            .write_to(&mut BufWriter::new(File::create(&local)?))?;

        Ok(())
    }

    fn purge_from_cache(&self, workspace: &Workspace) -> Result<(), Error> {
        let path = self.cache_path(workspace);
        if path.exists() {
            crate::utils::remove_file(&path)?;
        }
        Ok(())
    }

    fn copy_source_to(&self, workspace: &Workspace, dest: &Path) -> Result<(), Error> {
        let cached = self.cache_path(workspace);
        let mut file = File::open(cached)?;
        let mut tar = Archive::new(GzDecoder::new(BufReader::new(&mut file)));

        info!(
            "extracting crate {} {} into {}",
            self.name,
            self.version,
            dest.display()
        );
        if let Err(err) = unpack_without_first_dir(&mut tar, dest) {
            let _ = crate::utils::remove_dir_all(dest);
            Err(err
                .context(format!(
                    "unable to download {} version {}",
                    self.name, self.version
                ))
                .into())
        } else {
            Ok(())
        }
    }
}

impl std::fmt::Display for RegistryCrate {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{} crate {} {}",
            self.registry.name(),
            self.name,
            self.version
        )
    }
}

fn unpack_without_first_dir<R: Read>(archive: &mut Archive<R>, path: &Path) -> Result<(), Error> {
    let entries = archive.entries()?;
    for entry in entries {
        let mut entry = entry?;
        let relpath = {
            let path = entry.path();
            let path = path?;
            path.into_owned()
        };
        let mut components = relpath.components();
        // Throw away the first path component
        components.next();
        let full_path = path.join(&components.as_path());
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&full_path)?;
    }

    Ok(())
}
