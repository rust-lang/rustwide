#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use self::unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use self::windows::*;

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct CurrentUser {
    pub(crate) user_id: u32,
    pub(crate) group_id: u32,
}
