use super::CurrentUser;
use crate::cmd::KillFailedError;
use anyhow::anyhow;
use std::fs::File;
use std::path::Path;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

pub(crate) fn kill_process(id: u32) -> anyhow::Result<(), KillFailedError> {
    let error = Err(KillFailedError { pid: id });

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, id);
        if handle == 0 || handle == -1 {
            return error;
        }
        if TerminateProcess(handle, 101) == 0 {
            return error;
        }
        if CloseHandle(handle) == 0 {
            return error;
        }
    }

    Ok(())
}

pub(crate) fn current_user() -> Option<CurrentUser> {
    None
}

fn path_ends_in_exe<P: AsRef<Path>>(path: P) -> anyhow::Result<bool> {
    path.as_ref()
        .extension()
        .ok_or_else(|| anyhow!("Unable to get `Path` extension"))
        .map(|ext| ext == "exe")
}

/// Check that the file exists and has `.exe` as its extension.
pub(crate) fn is_executable<P: AsRef<Path>>(path: P) -> anyhow::Result<bool> {
    let path = path.as_ref();
    File::open(path)
        .map_err(Into::into)
        .and_then(|_| path_ends_in_exe(path))
}

pub(crate) fn make_executable<P: AsRef<Path>>(path: P) -> anyhow::Result<()> {
    if is_executable(path)? {
        Ok(())
    } else {
        anyhow::bail!("Downloaded binaries should be executable by default");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_kill_process() {
        // Try to kill a sleep command
        let mut cmd = Command::new("timeout").args(&["2"]).spawn().unwrap();
        kill_process(cmd.id()).unwrap();

        // Ensure it returns the code passed to `TerminateProcess`
        assert_eq!(cmd.wait().unwrap().code(), Some(101));
    }
}
