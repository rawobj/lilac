//! Letting a phone reach Soloist while it pairs.
//!
//! A Spotify app pairs Soloist by fetching its details over HTTP on a port
//! Soloist picks at random. Desktop firewalls such as ufw drop that by
//! default, so the phone never lists Soloist. Lilac opens that one port to
//! private networks for the pairing only, through the system's password
//! prompt (pkexec), and closes it again once Soloist is paired, after
//! fifteen minutes, or when Lilac exits, whichever comes first.

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context as _, Result, bail};

/// How long the port stays open at most.
const OPEN_SECS: u32 = 15 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ufw,
    Firewalld,
}

impl Kind {
    /// The command a user could run themselves, for when the password
    /// prompt is not available.
    pub fn manual_command(self, port: u16) -> String {
        match self {
            Kind::Ufw => {
                format!("sudo ufw allow proto tcp from 192.168.0.0/16 to any port {port}")
            }
            Kind::Firewalld => format!("sudo firewall-cmd --add-port={port}/tcp --timeout=900"),
        }
    }
}

/// The firewall that is turned on, if any Lilac knows how to open.
pub fn detect() -> Option<Kind> {
    // macOS asks about incoming connections by itself the first time
    // Soloist listens; Spotify builds Soloist for no other system.
    if !cfg!(target_os = "linux") {
        return None;
    }
    let active = |unit: &str| {
        Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .status()
            .is_ok_and(|s| s.success())
    };
    if active("ufw") && ufw_enabled() {
        Some(Kind::Ufw)
    } else if active("firewalld") {
        Some(Kind::Firewalld)
    } else {
        None
    }
}

/// The ufw service also runs while the firewall itself is switched off.
fn ufw_enabled() -> bool {
    std::fs::read_to_string("/etc/ufw/ufw.conf")
        .map(|conf| {
            conf.lines()
                .any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes"))
        })
        // Unreadable: assume it is on; opening a port that is open is harmless.
        .unwrap_or(true)
}

const UFW_SCRIPT: &str = r#"port=$1; secs=$2
nets="10.0.0.0/8 172.16.0.0/12 192.168.0.0/16"
for net in $nets; do
  ufw allow proto tcp from "$net" to any port "$port" comment 'Lilac: pairing Soloist' >/dev/null || exit 3
done
echo open
timeout "$secs" head -c 1 >/dev/null
for net in $nets; do
  ufw delete allow proto tcp from "$net" to any port "$port" >/dev/null 2>&1
done
"#;

const FIREWALLD_SCRIPT: &str = r#"port=$1; secs=$2
firewall-cmd --add-port="$port/tcp" --timeout="$secs" >/dev/null || exit 3
echo open
timeout "$secs" head -c 1 >/dev/null
firewall-cmd --remove-port="$port/tcp" >/dev/null 2>&1
"#;

/// An open port. Dropping it closes the port again: the helper waits on
/// its stdin and removes the rule once that closes, which also happens if
/// Lilac dies.
pub struct Opening {
    _stdin: ChildStdin,
    _child: Child,
}

/// Ask for the password and open `port`. Blocks until the rule is in place
/// or the prompt was dismissed; run it off the UI thread.
pub fn open(kind: Kind, port: u16) -> Result<Opening> {
    let script = match kind {
        Kind::Ufw => UFW_SCRIPT,
        Kind::Firewalld => FIREWALLD_SCRIPT,
    };
    let mut child = Command::new("pkexec")
        .args(["/bin/sh", "-c", script, "lilac-pairing"])
        .arg(port.to_string())
        .arg(OPEN_SECS.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("this system has no password prompt for apps (pkexec)")?;
    let stdin = child.stdin.take().context("no pipe to the helper")?;
    let stdout = child.stdout.take().context("no pipe from the helper")?;
    let mut line = String::new();
    let _ = BufReader::new(stdout).read_line(&mut line);
    if line.trim() == "open" {
        return Ok(Opening {
            _stdin: stdin,
            _child: child,
        });
    }
    let code = child.wait().ok().and_then(|s| s.code());
    match code {
        Some(126) => bail!("the password prompt was closed"),
        Some(127) => bail!("no password prompt is running on this desktop"),
        _ => bail!("the firewall did not accept the change"),
    }
}

/// The TCP port `pid` listens on for other machines (Soloist's pairing
/// port; its control socket listens on localhost only).
pub fn pairing_port(pid: u32) -> Option<u16> {
    let inodes: Vec<u64> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()?
        .filter_map(|entry| {
            let link = std::fs::read_link(entry.ok()?.path()).ok()?;
            let link = link.to_str()?;
            link.strip_prefix("socket:[")?
                .strip_suffix(']')?
                .parse()
                .ok()
        })
        .collect();
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .find_map(|table| listening_port(Path::new(table), &inodes))
}

fn listening_port(table: &Path, inodes: &[u64]) -> Option<u16> {
    let text = std::fs::read_to_string(table).ok()?;
    parse_listening(&text, inodes)
}

fn parse_listening(text: &str, inodes: &[u64]) -> Option<u16> {
    text.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (local, state, inode) = (fields.get(1)?, fields.get(3)?, fields.get(9)?);
        if *state != "0A" || !inodes.contains(&inode.parse().ok()?) {
            return None;
        }
        let (addr, port) = local.split_once(':')?;
        // Loopback only: 127.0.0.1 in /proc/net/tcp, ::1 in tcp6.
        let loopback = addr == "0100007F" || addr == "00000000000000000000000001000000";
        (!loopback).then(|| u16::from_str_radix(port, 16).ok())?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_port_open_to_other_machines() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:87EB 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0 100 0 0 10 0
   1: 00000000:D63B 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1 0 100 0 0 10 0
   2: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 333 1 0 100 0 0 10 0
";
        assert_eq!(parse_listening(table, &[111, 222]), Some(0xD63B));
        assert_eq!(parse_listening(table, &[111]), None);
        assert_eq!(parse_listening(table, &[]), None);
    }
}
