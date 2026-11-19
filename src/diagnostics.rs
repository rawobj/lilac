//! Logs, crash reports and test reports, for people testing Lilac to send
//! back without a terminal.
//!
//! - Everything Lilac and its libraries warn about goes to a small rotating
//!   log file (`<data dir>/logs/lilac.log`), as well as to stderr.
//! - A panic writes a crash report to `<data dir>/crashes/` before the
//!   process goes. A run that ends any other way (killed, a native crash)
//!   leaves its "running" marker behind; the next start notices both and
//!   offers the report.
//! - A test report (Settings) runs a few checks against Spotify and saves
//!   them with the recent log to the Downloads folder.
//!
//! Every report is anonymised before it is written: no account id, name,
//! email, tokens, keys or home folder. A random id tells reports apart.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Log lines kept in memory for reports.
const TAIL: usize = 300;
/// The log file is rotated past this size (one older file is kept).
const LOG_LIMIT: u64 = 2 * 1024 * 1024;

pub fn version() -> String {
    format!(
        "Lilac {} ({})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    )
}

pub fn logs_dir() -> PathBuf {
    crate::paths::data_dir().join("logs")
}

pub fn crashes_dir() -> PathBuf {
    crate::paths::data_dir().join("crashes")
}

fn marker() -> PathBuf {
    crate::paths::data_dir().join("running")
}

struct Log {
    tail: VecDeque<String>,
    file: Option<std::fs::File>,
    written: u64,
}

fn log_state() -> &'static Mutex<Log> {
    static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
    LOG.get_or_init(|| {
        let dir = logs_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("lilac.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT) {
            let _ = std::fs::rename(&path, dir.join("lilac.log.1"));
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        let written = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0);
        Mutex::new(Log {
            tail: VecDeque::with_capacity(TAIL),
            file,
            written,
        })
    })
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{} {h:02}:{m:02}:{s:02}Z", crate::diagnostics::date(secs))
}

/// `YYYY-MM-DD` (UTC) of Unix seconds.
pub fn date(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Write a line to stderr and the log.
pub fn note(line: &str) {
    eprintln!("{line}");
    let stamped = format!("{} {line}", timestamp());
    let Ok(mut log) = log_state().lock() else {
        return;
    };
    if log.tail.len() == TAIL {
        log.tail.pop_front();
    }
    log.tail.push_back(stamped.clone());
    let len = stamped.len() as u64 + 1;
    if log.written + len > LOG_LIMIT {
        // Start a fresh file; the old one stays as `.1`.
        let dir = logs_dir();
        let _ = std::fs::rename(dir.join("lilac.log"), dir.join("lilac.log.1"));
        log.file = std::fs::File::create(dir.join("lilac.log")).ok();
        log.written = 0;
    }
    if let Some(file) = log.file.as_mut()
        && writeln!(file, "{stamped}").is_ok()
    {
        log.written += len;
    }
}

/// `note` with `format!` arguments.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => {
        $crate::diagnostics::note(&format!($($arg)*))
    };
}

fn tail() -> Vec<String> {
    log_state()
        .lock()
        .map(|log| log.tail.iter().cloned().collect())
        .unwrap_or_default()
}

/// Warnings and errors from the libraries (librespot above all) go to the log.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            note(&format!(
                "{} {}: {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
    }

    fn flush(&self) {}
}

/// What there is to tell about the last run, found at start-up.
#[derive(Clone, Debug, PartialEq)]
pub enum LastRun {
    /// A crash report was written.
    Crashed(PathBuf),
    /// The run ended without closing (killed, or a crash that could not
    /// write a report). A report was made from its log.
    Vanished(PathBuf),
}

impl LastRun {
    pub fn path(&self) -> &Path {
        match self {
            LastRun::Crashed(path) | LastRun::Vanished(path) => path,
        }
    }
}

/// Copy a saved report to the Downloads folder, ready to send; and open
/// that folder. The copy's path.
pub fn hand_over(report: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(report).ok()?;
    let name = report.file_name()?.to_string_lossy().to_string();
    let saved = save_for_sending(&format!("lilac-{name}"), &text)?;
    if let Some(dir) = saved.parent() {
        let _ = open::that_detached(dir);
    }
    mark_seen(report);
    Some(saved)
}

static LAST_RUN: OnceLock<Option<LastRun>> = OnceLock::new();

/// What happened to the last run, if anything worth a word.
pub fn last_run() -> Option<LastRun> {
    LAST_RUN.get().cloned().flatten()
}

/// Start logging and crash reporting. Call first thing in `main`.
pub fn init() {
    let _ = log::set_boxed_logger(Box::new(Logger));
    log::set_max_level(log::LevelFilter::Warn);

    // The previous run: did it leave a report, or its marker?
    let dir = crashes_dir();
    let unseen = std::fs::read_dir(&dir).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "txt"))
            .filter(|p| !p.with_extension("seen").exists())
            .max()
    });
    let vanished = marker().exists();
    let last = match (unseen, vanished) {
        (Some(report), _) => Some(LastRun::Crashed(report)),
        (None, true) => vanished_report().map(LastRun::Vanished),
        (None, false) => None,
    };
    let _ = LAST_RUN.set(last);
    let _ = std::fs::create_dir_all(crate::paths::data_dir());
    let _ = std::fs::write(marker(), std::process::id().to_string());

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        let backtrace = std::backtrace::Backtrace::force_capture();
        note(&format!(
            "PANIC in thread '{thread}' at {location}: {message}"
        ));
        let mut body = String::new();
        let _ = writeln!(
            body,
            "What happened: Lilac stopped because of an error (a panic)."
        );
        let _ = writeln!(body, "Message: {message}");
        let _ = writeln!(body, "Where: {location} (thread '{thread}')");
        let _ = writeln!(body, "\nBacktrace:\n{backtrace}");
        let _ = write_report("crash", &body);
        // The report replaces the marker as the account of this run.
        let _ = std::fs::remove_file(marker());
        default_hook(info);
    }));
}

/// Lilac is closing normally.
pub fn clean_exit() {
    let _ = std::fs::remove_file(marker());
}

/// A report for a run that ended without closing, from its log.
fn vanished_report() -> Option<PathBuf> {
    let body = "What happened: the last time Lilac ran, it ended without closing \
                (it was killed, or crashed in a way that could not write a report). \
                The log from that run follows.\n";
    write_report("unexpected-exit", body)
}

/// Mark a crash report as seen, so it is offered once.
pub fn mark_seen(report: &Path) {
    let _ = std::fs::write(report.with_extension("seen"), b"");
}

/// Write an anonymised report into the crashes folder; its path.
fn write_report(kind: &str, body: &str) -> Option<PathBuf> {
    let dir = crashes_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{}-{kind}-{secs}.txt", date(secs)));
    let text = compose(kind, body, &log_file_tail());
    std::fs::write(&path, text).ok()?;
    prune(&dir, 20);
    Some(path)
}

/// The end of the log file on disk (the in-memory tail is empty at start-up).
fn log_file_tail() -> Vec<String> {
    let in_memory = tail();
    if !in_memory.is_empty() {
        return in_memory;
    }
    let raw = std::fs::read_to_string(logs_dir().join("lilac.log")).unwrap_or_default();
    let lines: Vec<&str> = raw.lines().collect();
    lines[lines.len().saturating_sub(TAIL)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut reports: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    reports.sort();
    let excess = reports.len().saturating_sub(keep);
    for old in reports.into_iter().take(excess) {
        let _ = std::fs::remove_file(old.with_extension("seen"));
        let _ = std::fs::remove_file(old);
    }
}

/// The header every report starts with, the body, and the log, anonymised.
fn compose(kind: &str, body: &str, log: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Lilac {kind} report {}", report_id());
    let _ = writeln!(out, "Version: {}", version());
    let _ = writeln!(out, "System: {}", system());
    let _ = writeln!(out, "Written: {}", timestamp());
    let _ = writeln!(out);
    out.push_str(body);
    let _ = writeln!(out, "\nRecent log ({} lines):", log.len());
    for line in log {
        let _ = writeln!(out, "  {line}");
    }
    anonymise(&out)
}

fn report_id() -> String {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The operating system and its version, in words.
pub fn system() -> String {
    let os = std::env::consts::OS;
    let detail = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|r| {
                r.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .unwrap_or_default()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .map(|o| format!("macOS {}", String::from_utf8_lossy(&o.stdout).trim()))
            .unwrap_or_default()
    } else {
        String::new()
    };
    if detail.is_empty() {
        format!("{os} {}", std::env::consts::ARCH)
    } else {
        format!("{detail} ({os} {})", std::env::consts::ARCH)
    }
}

/// Things that identify the user, set once they are known (account id,
/// name, email, the Soloist key).
static PRIVATE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Remember something that must never appear in a report.
pub fn private(value: &str) {
    let value = value.trim();
    if value.len() < 3 {
        return;
    }
    if let Ok(mut private) = PRIVATE.lock()
        && !private.iter().any(|p| p == value)
    {
        private.push(value.to_string());
    }
}

/// The signed-in account's id and name never appear in a report.
pub fn private_user(user: &crate::model::User) {
    private(&user.id);
    if let Some(name) = &user.display_name {
        private(name);
    }
}

/// Take out what identifies the user: their known ids and names, the home
/// folder, emails, bearer tokens and anything shaped like a secret (long
/// runs of letters, digits, `-` and `_`).
pub fn anonymise(text: &str) -> String {
    let mut out = text.to_string();
    if let Ok(private) = PRIVATE.lock() {
        for value in private.iter() {
            out = out.replace(value.as_str(), "<private>");
        }
    }
    for home in [
        std::env::var("HOME").ok(),
        std::env::var("USERPROFILE").ok(),
    ]
    .into_iter()
    .flatten()
    .filter(|h| h.len() > 3)
    {
        out = out.replace(&home, "~");
    }
    for user in [std::env::var("USER").ok(), std::env::var("USERNAME").ok()]
        .into_iter()
        .flatten()
        .filter(|u| u.len() > 2)
    {
        out = out.replace(&user, "<user>");
    }
    out.lines().map(scrub_line).collect::<Vec<_>>().join("\n") + "\n"
}

fn scrub_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.contains('@') && word.contains('.') {
            out.push_str("<email>");
        } else if word.len() >= 32 {
            out.push_str("<secret>");
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    let mut bearer = false;
    for c in line.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '@' | '.' | '+' | '/' | '=') {
            word.push(c);
        } else {
            if bearer && !word.is_empty() {
                word = "<secret>".into();
            }
            bearer = word.eq_ignore_ascii_case("bearer");
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    if bearer && !word.is_empty() {
        word = "<secret>".into();
    }
    flush(&mut word, &mut out);
    out
}

/// Save a report where the user can find it (Downloads, else the home
/// folder) under a name that says what it is. The saved path.
pub fn save_for_sending(name: &str, text: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let downloads = std::env::var_os("XDG_DOWNLOAD_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| home.join("Downloads"));
    let dir = if downloads.is_dir() { downloads } else { home };
    let path = dir.join(name);
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// A new test report: the checks' results and the recent log, anonymised.
pub fn test_report(checks: &str) -> String {
    compose("test", checks, &tail())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_leave_out_who_the_user_is() {
        private("31zg5g75cp2elc5fzzibfqr3ka3q");
        private("resunga");
        let text = anonymise(
            "user 31zg5g75cp2elc5fzzibfqr3ka3q (resunga) mail a.b@c.com \
             Authorization: Bearer abc.def key=0123456789abcdef0123456789abcdef0123",
        );
        assert!(!text.contains("31zg5g75"));
        assert!(!text.contains("resunga"));
        assert!(!text.contains("a.b@c.com"));
        assert!(!text.contains("abc.def"));
        assert!(!text.contains("0123456789abcdef0123456789abcdef"));
        assert!(text.contains("<private>"));
        // Ordinary words and short ids stay readable.
        assert!(anonymise("library service failed: 400 Bad Request").contains("400 Bad Request"));
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_794_980_862), "2026-11-18");
    }
}
