//! Starting helper programs out of sight.

use std::ffi::OsStr;
use std::process::Command;

/// A command for a console program run in the background. Lilac is a windowed
/// program on Windows, so each console program it starts would otherwise open
/// a console window of its own (Soloist's would stay up as long as it runs).
pub fn background(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
