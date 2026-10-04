//! Owned agent processes.
pub use std::process::Stdio;
pub use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[cfg(unix)]
pub(crate) fn signal_target(child: &Child) -> Option<i32> {
    let pid = child.id()? as i32;
    // SAFETY: getpgid only inspects the owned, unreaped child.
    Some(if unsafe { libc::getpgid(pid) } == pid {
        -pid
    } else {
        pid
    })
}
