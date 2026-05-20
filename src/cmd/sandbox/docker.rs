use crate::{Workspace, cmd::Command};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// discovered cgroup version on the host.
/// Most of the time: v2
/// on old systems like the docs.rs builder: v1
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CgroupVersion {
    V1,
    V2,
    Unavailable,
}

/// State of the host cgroup files, after discovery.
///
/// This is for discovering OOM counts & memory peaks
/// without having to call `docker exec cat`.
/// Might be extended for more systems / hosts / docker
/// engines when necessary.
#[derive(Debug)]
pub(super) enum HostCgroupState {
    /// didn't try yet
    Unknown,
    /// tried and succeeded
    Available(HostCgroup),
    /// tried and failed
    Unavailable,
}

/// result of the host-cgroup-discovery, specific to one
/// started docker container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HostCgroup {
    pub(super) version: CgroupVersion,
    pub(super) memory_peak_file: PathBuf,
    pub(super) oom_kill_count_file: PathBuf,
}

impl HostCgroup {
    /// Resolve the container's host-visible cgroup files from its host PID.
    ///
    /// This is the fast path: it avoids spawning `docker exec cat` for every
    /// stats read by mapping `/proc/<pid>/cgroup` to candidate files under the
    /// host's `/sys/fs/cgroup`, then choosing the first usable candidate whose
    /// expected files exist.
    ///
    /// Some hosts expose both a v2 hierarchy entry (`0::...`) and a v1 memory
    /// controller entry (`memory:...`). In that hybrid case we prefer the v1
    /// memory candidate first and only fall back to the v2 candidate if the v1
    /// files are not actually present.
    ///
    /// The result is best-effort. Some nested-container setups do not expose a
    /// usable host `/proc` or cgroup mount, and callers must fall back to the
    /// in-container reads when this returns `None`.
    pub(super) fn detect(pid: u32) -> Option<Self> {
        let proc_cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;

        Self::choose_usable(Self::parse(proc_cgroup.lines()), |path| path.exists())
    }

    /// Parse `/proc/<pid>/cgroup` and build host-side cgroup candidates.
    ///
    /// Each line in `/proc/<pid>/cgroup` has three colon-separated fields:
    ///
    /// `hierarchy-ID:controller-list:cgroup-path`
    ///
    /// For cgroups v2, the hierarchy ID is `0` and the controller list is
    /// empty, so lines look like `0::/some/path`. That cgroup path is relative
    /// to the v2 mount point at `/sys/fs/cgroup`.
    ///
    /// For cgroups v1, the controller list names the controllers attached to
    /// that hierarchy, such as `memory,cpu`. We only care about the entry whose
    /// controller list contains `memory`, because that is where both peak memory
    /// accounting and the `oom_kill` counter live. The cgroup path from that
    /// entry is relative to the v1 memory controller mount point at
    /// `/sys/fs/cgroup/memory`.
    ///
    /// This method does not choose a final host cgroup on its own. Instead it
    /// constructs zero, one, or two ordered candidates:
    ///
    /// - first the v1 memory-controller candidate, if present
    /// - then the v2 candidate, if present
    ///
    /// That ordering matters: callers can prefer the v1 memory hierarchy on
    /// hybrid hosts while still falling back to v2 if the v1 files are absent.
    ///
    /// For each discovered hierarchy, the candidate contains the concrete
    /// host-side files used by rustwide:
    ///
    /// - v2: `memory.peak` and `memory.events`
    /// - v1: `memory.max_usage_in_bytes` and `memory.oom_control`
    ///
    /// related docs:
    /// * https://www.kernel.org/doc/Documentation/cgroup-v1/memory.txt
    /// * https://docs.kernel.org/admin-guide/cgroup-v2.html
    pub(super) fn parse<'a, I>(proc_cgroup: I) -> Vec<HostCgroup>
    where
        I: IntoIterator<Item = &'a str>,
    {
        proc_cgroup
            .into_iter()
            .filter_map(|line| {
                // we have three elements on each line, separated by `:`
                let mut parts = line.splitn(3, ':');

                if let (Some(_hierarchy), Some(controllers), Some(path)) =
                    (parts.next(), parts.next(), parts.next())
                {
                    // we only care about controllers & the path
                    Some((controllers, path))
                } else {
                    None
                }
            })
            .filter_map(|(controllers, path)| {
                if controllers.is_empty() {
                    let base = Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'));
                    Some(HostCgroup {
                        version: CgroupVersion::V2,
                        memory_peak_file: base.join("memory.peak"),
                        oom_kill_count_file: base.join("memory.events"),
                    })
                } else if controllers
                    .split(',')
                    .any(|controller| controller == "memory")
                {
                    let base =
                        Path::new("/sys/fs/cgroup/memory").join(path.trim_start_matches('/'));
                    Some(HostCgroup {
                        version: CgroupVersion::V1,
                        memory_peak_file: base.join("memory.max_usage_in_bytes"),
                        oom_kill_count_file: base.join("memory.oom_control"),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Choose the first candidate whose required files both exist.
    ///
    /// Candidates are tried in the order produced by [`HostCgroup::parse`], so
    /// it's depending on the order in the cgroup file itself.
    /// Some tests showed V1 being first, and then V2.
    fn choose_usable<I>(candidates: I, exists: impl Fn(&Path) -> bool) -> Option<HostCgroup>
    where
        I: IntoIterator<Item = HostCgroup>,
    {
        candidates.into_iter().find(|candidate| {
            exists(&candidate.memory_peak_file) && exists(&candidate.oom_kill_count_file)
        })
    }

    /// Read the host-side peak memory file for this container.
    ///
    /// This intentionally mirrors the in-container fallback read so the caller
    /// can compare the two paths in debug builds.
    pub(super) fn read_memory_peak(&self) -> Option<u64> {
        parse_memory_peak(fs::read_to_string(&self.memory_peak_file).ok()?.lines())
    }

    /// Read the host-side `oom_kill` counter for this container.
    ///
    /// Like [`HostCgroup::read_memory_peak`], this stays small and best-effort
    /// so the caller can cheaply prefer host reads and fall back when needed.
    pub(super) fn read_oom_kill_count(&self) -> Option<u64> {
        Some(parse_oom_kill_count(
            fs::read_to_string(&self.oom_kill_count_file).ok()?.lines(),
        ))
    }
}

/// Parse the memory peak value from a cgroup file, inside the container, or on the host.
pub(super) fn parse_memory_peak<I, S>(lines: I) -> Option<u64>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    lines
        .into_iter()
        .next()?
        .as_ref()
        .trim()
        .parse::<u64>()
        .ok()
}

/// Parse the `oom_kill` counter from a cgroup events file.
///
/// The v1 and v2 files differ in path but both expose `oom_kill <count>` in
/// their contents. If the file is readable but the key is missing, treat it as
/// zero rather than as a hard failure.
pub(super) fn parse_oom_kill_count<I, S>(lines: I) -> u64
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    lines
        .into_iter()
        .filter_map(|line| {
            line.as_ref()
                .strip_prefix("oom_kill ")
                .and_then(|rest| rest.trim().parse::<u64>().ok())
        })
        .next()
        .unwrap_or(0)
}

/// groups all functionality around reading from the cgroup / docker statistics
/// for a running container.
pub(super) struct CgroupStatsReader<'w> {
    oom_kill_count: Option<u64>,
    cgroup_version: Option<CgroupVersion>,
    host_cgroup: HostCgroupState,
    workspace: &'w Workspace,
    container_id: String,
    pub(super) pid: Option<u32>,
}

impl<'w> CgroupStatsReader<'w> {
    pub(super) fn new(workspace: &'w Workspace, container_id: impl Into<String>) -> Self {
        Self {
            oom_kill_count: None,
            cgroup_version: None,
            host_cgroup: HostCgroupState::Unknown,
            workspace,
            container_id: container_id.into(),
            pid: None,
        }
    }

    fn exec_cat_file(&self, path: &str) -> Option<Vec<String>> {
        Command::new(self.workspace, "docker")
            .args(["exec", &self.container_id, "cat", path])
            .log_output(false)
            .log_command(false)
            .run_capture()
            .ok()
            .map(|o| o.stdout_lines().to_vec())
    }

    /// Read a cgroup file from inside the container using the cached cgroup
    /// flavor when known, otherwise probe v2 first and then v1.
    ///
    /// This remains the compatibility path for environments where host-side
    /// cgroup access is unavailable. It also establishes the cached cgroup
    /// version so later reads avoid probing both hierarchies again.
    fn exec_cat_cgroup_file(&mut self, v2_path: &str, v1_path: &str) -> Option<Vec<String>> {
        match self.cgroup_version {
            Some(CgroupVersion::V2) => self.exec_cat_file(v2_path),
            Some(CgroupVersion::V1) => self.exec_cat_file(v1_path),
            Some(CgroupVersion::Unavailable) => None,
            None => {
                if let Some(lines) = self.exec_cat_file(v2_path) {
                    self.cgroup_version = Some(CgroupVersion::V2);
                    Some(lines)
                } else if let Some(lines) = self.exec_cat_file(v1_path) {
                    self.cgroup_version = Some(CgroupVersion::V1);
                    Some(lines)
                } else {
                    self.cgroup_version = Some(CgroupVersion::Unavailable);
                    None
                }
            }
        }
    }

    pub(super) fn read_memory_peak_from_container(&mut self) -> Option<u64> {
        self.exec_cat_cgroup_file(
            "/sys/fs/cgroup/memory.peak",
            "/sys/fs/cgroup/memory/memory.max_usage_in_bytes",
        )
        .and_then(parse_memory_peak)
    }

    pub(super) fn read_oom_kill_count_from_container(&mut self) -> Option<u64> {
        Some(parse_oom_kill_count(self.exec_cat_cgroup_file(
            "/sys/fs/cgroup/memory.events",
            "/sys/fs/cgroup/memory/memory.oom_control",
        )?))
    }

    pub(super) fn detect_host_cgroup(&mut self) -> Option<&HostCgroup> {
        if matches!(self.host_cgroup, HostCgroupState::Unknown) {
            self.host_cgroup = match self.pid.and_then(HostCgroup::detect) {
                Some(host_cgroup) => {
                    self.cgroup_version = Some(host_cgroup.version);
                    HostCgroupState::Available(host_cgroup)
                }
                None => HostCgroupState::Unavailable,
            };
        }

        match &self.host_cgroup {
            HostCgroupState::Available(host_cgroup) => Some(host_cgroup),
            HostCgroupState::Unavailable | HostCgroupState::Unknown => None,
        }
    }

    pub(super) fn record_oom_kill_count(&mut self) {
        self.oom_kill_count = self.read_oom_kill_count();
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub(super) fn read_memory_peak(&mut self) -> Option<u64> {
        if let Some(host_cgroup) = self.detect_host_cgroup()
            && let Some(peak) = host_cgroup.read_memory_peak()
        {
            Some(peak)
        } else {
            self.read_memory_peak_from_container()
        }
    }

    pub(super) fn read_oom_kill_count(&mut self) -> Option<u64> {
        if let Some(host_cgroup) = self.detect_host_cgroup()
            && let Some(count) = host_cgroup.read_oom_kill_count()
        {
            Some(count)
        } else {
            self.read_oom_kill_count_from_container()
        }
    }

    pub(super) fn check_cgroup_oom(&mut self) -> bool {
        let current = self.read_oom_kill_count();
        let previous = self.oom_kill_count;
        self.oom_kill_count = current;

        current.unwrap_or_default() > previous.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn host_cgroup(
        version: CgroupVersion,
        memory_peak_file: &str,
        oom_kill_count_file: &str,
    ) -> HostCgroup {
        HostCgroup {
            version,
            memory_peak_file: PathBuf::from(memory_peak_file),
            oom_kill_count_file: PathBuf::from(oom_kill_count_file),
        }
    }

    fn parse<'a, I>(proc_cgroup: I) -> Vec<HostCgroup>
    where
        I: IntoIterator<Item = &'a str>,
    {
        HostCgroup::parse(proc_cgroup)
    }

    fn parse_one<'a, I>(proc_cgroup: I) -> HostCgroup
    where
        I: IntoIterator<Item = &'a str>,
    {
        let candidates = parse(proc_cgroup);
        assert_eq!(candidates.len(), 1);
        candidates.into_iter().next().unwrap()
    }

    #[test]
    fn parse_host_cgroup_v2() {
        assert_eq!(
            parse_one(["0::/docker/abc123"]),
            host_cgroup(
                CgroupVersion::V2,
                "/sys/fs/cgroup/docker/abc123/memory.peak",
                "/sys/fs/cgroup/docker/abc123/memory.events",
            )
        );
    }

    #[test]
    fn parse_host_cgroup_v1_memory_not_first() {
        assert_eq!(
            parse_one(["12:cpu,memory,cpuset:/docker/abc123"]),
            host_cgroup(
                CgroupVersion::V1,
                "/sys/fs/cgroup/memory/docker/abc123/memory.max_usage_in_bytes",
                "/sys/fs/cgroup/memory/docker/abc123/memory.oom_control",
            )
        );
    }

    #[test]
    fn parse_host_cgroup_v1() {
        assert_eq!(
            parse_one(["12:memory,cpu:/docker/abc123"]),
            host_cgroup(
                CgroupVersion::V1,
                "/sys/fs/cgroup/memory/docker/abc123/memory.max_usage_in_bytes",
                "/sys/fs/cgroup/memory/docker/abc123/memory.oom_control",
            )
        );
    }

    #[test]
    fn parse_host_cgroup_ignores_non_memory_v1_lines() {
        assert!(parse(["9:cpu,cpuacct:/docker/abc123"]).is_empty());
    }

    #[test]
    fn parse_host_cgroup_prefers_v1_over_v2() {
        assert_eq!(
            parse([
                "12:memory,cpu:/docker/old",
                "0::/docker/new",
                "5:cpuacct,cpu:/docker/ignored",
            ]),
            vec![
                host_cgroup(
                    CgroupVersion::V1,
                    "/sys/fs/cgroup/memory/docker/old/memory.max_usage_in_bytes",
                    "/sys/fs/cgroup/memory/docker/old/memory.oom_control",
                ),
                host_cgroup(
                    CgroupVersion::V2,
                    "/sys/fs/cgroup/docker/new/memory.peak",
                    "/sys/fs/cgroup/docker/new/memory.events",
                ),
            ]
        );
    }

    #[test]
    fn parse_host_cgroup_returns_none_without_memory_or_v2() {
        assert!(parse(["9:cpu,cpuacct:/docker/abc123", "11:cpuset:/docker/abc123"]).is_empty());
    }

    #[test]
    fn choose_usable_prefers_first_when_both_candidates_exist() {
        let candidates = parse(["7:memory:/docker/v1", "0::/docker/v2"]);

        let chosen = HostCgroup::choose_usable(candidates, |path| {
            path == "/sys/fs/cgroup/memory/docker/v1/memory.max_usage_in_bytes"
                || path == "/sys/fs/cgroup/memory/docker/v1/memory.oom_control"
                || path == "/sys/fs/cgroup/docker/v2/memory.peak"
                || path == "/sys/fs/cgroup/docker/v2/memory.events"
        })
        .unwrap();

        assert_eq!(
            chosen,
            host_cgroup(
                CgroupVersion::V1,
                "/sys/fs/cgroup/memory/docker/v1/memory.max_usage_in_bytes",
                "/sys/fs/cgroup/memory/docker/v1/memory.oom_control",
            )
        );
    }

    #[test]
    fn choose_usable_falls_back_to_second_when_first_files_are_missing() {
        let candidates = parse(["7:memory:/docker/v1", "0::/docker/v2"]);

        let chosen = HostCgroup::choose_usable(candidates, |path| {
            path == "/sys/fs/cgroup/docker/v2/memory.peak"
                || path == "/sys/fs/cgroup/docker/v2/memory.events"
        })
        .unwrap();

        assert_eq!(
            chosen,
            host_cgroup(
                CgroupVersion::V2,
                "/sys/fs/cgroup/docker/v2/memory.peak",
                "/sys/fs/cgroup/docker/v2/memory.events",
            )
        );
    }

    #[test]
    fn choose_usable_returns_none_when_no_candidate_files_exist() {
        let candidates = parse(["7:memory:/docker/v1", "0::/docker/v2"]);

        let chosen = HostCgroup::choose_usable(candidates, |_| false);

        assert!(chosen.is_none());
    }

    #[test]
    fn parse_memory_peak_from_lines() {
        assert_eq!(parse_memory_peak(["12345"]), Some(12345));
    }

    #[test]
    fn parse_oom_kill_count_from_content() {
        assert_eq!(
            parse_oom_kill_count(["low 0", "high 0", "max 0", "oom 0", "oom_kill 7"]),
            7
        );
    }

    #[test]
    fn parse_oom_kill_count_defaults_to_zero_when_present_without_counter() {
        assert_eq!(parse_oom_kill_count(["under_oom 0", "oom 0"]), 0);
    }

    #[test]
    fn parse_example_from_docsrs_server() {
        assert_eq!(
            parse([
                "12:hugetlb:/docker/1",
                "11:perf_event:/docker/2",
                "10:net_cls,net_prio:/docker/3",
                "9:pids:/docker/4",
                "8:blkio:/docker/5",
                "7:memory:/docker/6",
                "6:cpuset:/docker/7",
                "5:rdma:/docker/8",
                "4:cpu,cpuacct:/docker/9",
                "3:devices:/docker/10",
                "2:freezer:/docker/11",
                "1:name=systemd:/docker/12",
                "0::/docker/13",
            ]),
            vec![
                host_cgroup(
                    CgroupVersion::V1,
                    "/sys/fs/cgroup/memory/docker/6/memory.max_usage_in_bytes",
                    "/sys/fs/cgroup/memory/docker/6/memory.oom_control",
                ),
                host_cgroup(
                    CgroupVersion::V2,
                    "/sys/fs/cgroup/docker/13/memory.peak",
                    "/sys/fs/cgroup/docker/13/memory.events",
                ),
            ]
        );
    }
}
