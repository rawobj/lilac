//! One Lilac at a time: starting it again brings the running window to the
//! front instead of opening a second one, which would log in twice and
//! fight the first over playback.
//!
//! The first instance listens on a local socket (a Unix socket, or a
//! loopback TCP port named in a file on Windows); a later one connects,
//! says "show" and exits. On Windows a named mutex also tells a later one
//! that a Lilac runs even when the port cannot be reached.

use std::io::{BufRead as _, BufReader};

use tokio::sync::mpsc;

const SHOW: &str = "lilac show";
const SHOWN: &str = "lilac ok";

/// Requests from later starts to show the window.
pub type Requests = mpsc::UnboundedReceiver<()>;

/// `None` when another instance is running and has been asked to show
/// itself: this one should exit.
pub fn claim() -> Option<Requests> {
    if other_alive() {
        // It may still be starting and not listening yet; it shows itself
        // anyway once it is up, so asking is only to bring it forward.
        for _ in 0..20 {
            if ask_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        return None;
    }
    if ask_running() {
        return None;
    }
    let (send, requests) = mpsc::unbounded_channel();
    // Not being able to listen only costs the single-instance behaviour.
    let _ = listen(send);
    Some(requests)
}

fn answer(mut stream: impl std::io::Read + std::io::Write, send: &mpsc::UnboundedSender<()>) {
    let mut line = String::new();
    if BufReader::new(&mut stream).read_line(&mut line).is_ok() && line.trim() == SHOW {
        let _ = writeln!(stream, "{SHOWN}");
        let _ = send.send(());
    }
}

fn ask(mut stream: impl std::io::Read + std::io::Write) -> bool {
    if writeln!(stream, "{SHOW}").is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).is_ok() && line.trim() == SHOWN
}

#[cfg(unix)]
mod imp {
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn socket() -> PathBuf {
        // The runtime dir is per user and cleared at logout; elsewhere the
        // user id keeps users apart.
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) => PathBuf::from(dir).join("lilac.sock"),
            None => std::env::temp_dir().join(format!("lilac-{}.sock", unsafe { libc::getuid() })),
        }
    }

    /// The socket's answer decides on Unix.
    pub fn other_alive() -> bool {
        false
    }

    pub fn ask_running() -> bool {
        let Ok(stream) = UnixStream::connect(socket()) else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        ask(stream)
    }

    pub fn listen(send: mpsc::UnboundedSender<()>) -> std::io::Result<()> {
        let path = socket();
        // Left behind by an instance that did not exit cleanly; nothing
        // answered on it above.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    answer(stream, &send);
                }
            })?;
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::time::Duration;

    use super::*;

    fn port_file() -> std::path::PathBuf {
        crate::paths::data_dir().join("instance.port")
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateMutexW(
            attributes: *const std::ffi::c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut std::ffi::c_void;
        fn GetLastError() -> u32;
    }

    const ERROR_ALREADY_EXISTS: u32 = 183;

    /// A named mutex Windows keeps while any Lilac of this user's session
    /// runs, and drops with the process however it ends. The port file
    /// alone can name a dead port (an instance killed, or overwritten by
    /// one that started next to it), and then a second Lilac would run.
    pub fn other_alive() -> bool {
        let name: Vec<u16> = "Local\\lilac-single-instance"
            .encode_utf16()
            .chain([0])
            .collect();
        // The handle stays open for the life of the process on purpose.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        !handle.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS
    }

    pub fn ask_running() -> bool {
        let Some(port) = std::fs::read_to_string(port_file())
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
        else {
            return false;
        };
        let addr = (Ipv4Addr::LOCALHOST, port).into();
        let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        ask(stream)
    }

    pub fn listen(send: mpsc::UnboundedSender<()>) -> std::io::Result<()> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        std::fs::create_dir_all(crate::paths::data_dir())?;
        std::fs::write(port_file(), port.to_string())?;
        std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    answer(stream, &send);
                }
            })?;
        Ok(())
    }
}

use imp::{ask_running, listen, other_alive};
