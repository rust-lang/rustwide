use super::CurrentUser;
use crate::cmd::KillFailedError;
use failure::Error;
use nix::{
    sys::signal::{kill, Signal},
    unistd::{Gid, Pid, Uid},
};
use std::convert::AsRef;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

const EXECUTABLE_BITS: u32 = 0o5;

pub(crate) fn kill_process(id: u32) -> Result<(), KillFailedError> {
    match kill(Pid::from_raw(id as i32), Signal::SIGKILL) {
        Ok(()) => Ok(()),
        Err(err) => Err(KillFailedError {
            pid: id,
            errno: if let nix::Error::Sys(errno) = err {
                Some(errno)
            } else {
                None
            },
        }),
    }
}

#[allow(clippy::unnecessary_wraps)] // the API is intentionally the same as `windows::current_user`
pub(crate) fn current_user() -> Option<CurrentUser> {
    Some(CurrentUser {
        user_id: Uid::effective().into(),
        group_id: Gid::effective().into(),
    })
}

fn executable_mode_for(path: &Path) -> Result<u32, Error> {
    let metadata = path.metadata()?;

    let user = current_user().unwrap();

    if metadata.uid() == user.user_id {
        Ok(EXECUTABLE_BITS << 6)
    } else if metadata.gid() == user.group_id {
        Ok(EXECUTABLE_BITS << 3)
    } else {
        Ok(EXECUTABLE_BITS)
    }
}

pub(crate) fn is_executable<P: AsRef<Path>>(path: P) -> Result<bool, Error> {
    let path = path.as_ref();

    let expected_mode = executable_mode_for(path)?;
    Ok(path.metadata()?.mode() & expected_mode == expected_mode)
}

pub(crate) fn make_executable<P: AsRef<Path>>(path: P) -> Result<(), Error> {
    let path = path.as_ref();

    // Set the executable and readable bits on the file
    let mut perms = path.metadata()?.permissions();
    let new_mode = perms.mode() | executable_mode_for(path)?;
    perms.set_mode(new_mode);

    ::std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CurrentUser;
    use nix::unistd::{Gid, Uid};
    use std::fs::File;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    #[test]
    fn test_kill_process() {
        // Try to kill a sleep command
        let mut cmd = Command::new("sleep").args(&["2"]).spawn().unwrap();
        super::kill_process(cmd.id()).unwrap();

        // Ensure it was killed with SIGKILL
        assert_eq!(cmd.wait().unwrap().signal(), Some(9));
    }

    #[test]
    fn test_current_user() {
        assert_eq!(
            super::current_user(),
            Some(CurrentUser {
                user_id: u32::from(Uid::effective()),
                group_id: u32::from(Gid::effective()),
            })
        );
    }

    #[test]
    fn test_executables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test");

        // Create the temp file and make sure it's not executable
        File::create(&path).unwrap();
        assert!(!super::is_executable(&path).unwrap());

        // And then make it executable
        super::make_executable(&path).unwrap();
        assert!(super::is_executable(&path).unwrap());
    }
}
