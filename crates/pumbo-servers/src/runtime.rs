//! How server processes run: the [`Runner`] trait and [`ProcessRunner`]
//! (plain child processes). Every action goes by the PID of the process,
//! never by a program name.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Program and arguments, run in the server's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// How a process ended: its exit code, `None` after a signal or when unknown.
pub type Exit = Option<i32>;

/// A started process.
#[derive(Debug)]
pub struct Started {
    pub pid: u32,
    /// Fires once when the process ends.
    pub exited: oneshot::Receiver<Exit>,
}

/// A living process seen by [`Runner::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStatus {
    /// Program name without its folder (`pumpkin`, `java`).
    pub name: String,
    /// Resident memory in KiB, when known.
    pub rss_kib: Option<u64>,
}

pub trait Runner: Send + Sync {
    /// Starts `launch` in `dir`; stdout and stderr go to `log`.
    fn start(&self, dir: &Path, launch: &Launch, log: Arc<Log>) -> Result<Started, String>;
    /// Asks a process to stop: `stop` on its console, a SIGTERM to a
    /// process without one (taken over after a proxy crash).
    fn stop(&self, pid: u32) -> Result<(), String>;
    fn kill(&self, pid: u32) -> Result<(), String>;
    fn write_stdin(&self, pid: u32, line: &str) -> Result<(), String>;
    /// `None` when no process has this PID.
    fn status(&self, pid: u32) -> Option<ProcStatus>;
}

/// Lines of a console kept for `logs`.
const LOG_LINES: usize = 500;
/// `console.log` becomes `console.log.1` at this size.
const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;

/// A server's console: `console.log` in its folder and the last lines.
#[derive(Debug)]
pub struct Log {
    path: PathBuf,
    lines: Mutex<VecDeque<String>>,
}

impl Log {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lines: Mutex::new(VecDeque::new()),
        }
    }

    /// Appends a line. ponytail: a blocking append per line; servers print
    /// little, a writer task when that shows up in profiles.
    pub fn push(&self, line: &str) {
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{line}");
        }
        if let Ok(mut lines) = self.lines.lock() {
            if lines.len() == LOG_LINES {
                lines.pop_front();
            }
            lines.push_back(strip_ansi(line));
        }
    }

    /// The last `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<String> {
        self.lines
            .lock()
            .map(|l| l.iter().skip(l.len().saturating_sub(n)).cloned().collect())
            .unwrap_or_default()
    }
}

/// Without terminal colour sequences (`ESC [ … letter`).
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

struct Child {
    stdin: mpsc::UnboundedSender<String>,
    kill: Option<oneshot::Sender<()>>,
}

/// Servers as child processes of the proxy.
#[derive(Default)]
pub struct ProcessRunner {
    children: Arc<Mutex<HashMap<u32, Child>>>,
}

impl std::fmt::Debug for ProcessRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessRunner").finish_non_exhaustive()
    }
}

fn pipe(from: impl AsyncRead + Unpin + Send + 'static, log: Arc<Log>) {
    tokio::spawn(async move {
        let mut r = BufReader::new(from);
        let mut buf = Vec::new();
        while r.read_until(b'\n', &mut buf).await.is_ok_and(|n| n > 0) {
            let line = String::from_utf8_lossy(&buf);
            log.push(line.trim_end_matches(['\r', '\n']));
            buf.clear();
        }
    });
}

impl Runner for ProcessRunner {
    fn start(&self, dir: &Path, launch: &Launch, log: Arc<Log>) -> Result<Started, String> {
        let mut cmd = tokio::process::Command::new(&launch.program);
        cmd.args(&launch.args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group: Ctrl+C in the proxy's terminal does not reach
        // the servers, the proxy stops them itself.
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{}: {e}", launch.program.display()))?;
        let pid = child.id().ok_or("the process ended at once")?;
        if let Some(out) = child.stdout.take() {
            pipe(out, log.clone());
        }
        if let Some(err) = child.stderr.take() {
            pipe(err, log);
        }
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                while let Some(line) = stdin_rx.recv().await {
                    let ok = stdin
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_ok()
                        && stdin.flush().await.is_ok();
                    if !ok {
                        break;
                    }
                }
            });
        }
        let (kill_tx, kill_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = oneshot::channel();
        if let Ok(mut c) = self.children.lock() {
            c.insert(
                pid,
                Child {
                    stdin: stdin_tx,
                    kill: Some(kill_tx),
                },
            );
        }
        let children = self.children.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                s = child.wait() => s,
                _ = kill_rx => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            if let Ok(mut c) = children.lock() {
                c.remove(&pid);
            }
            let _ = exit_tx.send(status.ok().and_then(|s| s.code()));
        });
        Ok(Started {
            pid,
            exited: exit_rx,
        })
    }

    fn stop(&self, pid: u32) -> Result<(), String> {
        if self.children.lock().is_ok_and(|c| c.contains_key(&pid)) {
            return self.write_stdin(pid, "stop");
        }
        signal(pid, "-TERM")
    }

    fn kill(&self, pid: u32) -> Result<(), String> {
        let own = self
            .children
            .lock()
            .ok()
            .and_then(|mut c| c.get_mut(&pid).map(|c| c.kill.take()));
        match own {
            Some(Some(tx)) => {
                let _ = tx.send(());
                Ok(())
            }
            Some(None) => Ok(()),
            None => signal(pid, "-KILL"),
        }
    }

    fn write_stdin(&self, pid: u32, line: &str) -> Result<(), String> {
        let c = self.children.lock().map_err(|_| "lock")?;
        let child = c
            .get(&pid)
            .ok_or("no console (not started by this proxy)")?;
        child
            .stdin
            .send(line.to_string())
            .map_err(|_| "the console is closed".to_string())
    }

    fn status(&self, pid: u32) -> Option<ProcStatus> {
        ps_status(pid)
    }
}

/// `kill <sig> <pid>` for a process that is not our child (one PID only).
#[cfg(unix)]
fn signal(pid: u32, sig: &str) -> Result<(), String> {
    let ok = std::process::Command::new("kill")
        .args([sig, &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Ok(())
    } else {
        Err(format!("kill {sig} {pid} failed"))
    }
}

#[cfg(not(unix))]
fn signal(pid: u32, _: &str) -> Result<(), String> {
    Err(format!("process {pid} was not started by this proxy"))
}

/// Name and memory of a process from `ps` (no command lines).
#[cfg(unix)]
fn ps_status(pid: u32) -> Option<ProcStatus> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "rss=,comm="])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_ps(&String::from_utf8_lossy(&out.stdout))
}

// ponytail: no process names on Windows yet, so servers left by a crashed
// proxy count as stopped there; tasklist when someone runs it on Windows.
#[cfg(not(unix))]
fn ps_status(_: u32) -> Option<ProcStatus> {
    None
}

/// `"  1234 /path/to/pumpkin"` → name `pumpkin`, 1234 KiB.
pub fn parse_ps(out: &str) -> Option<ProcStatus> {
    let line = out.lines().find(|l| !l.trim().is_empty())?.trim();
    let (rss, comm) = line.split_once(char::is_whitespace)?;
    let comm = comm.trim();
    let name = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    (!name.is_empty()).then(|| ProcStatus {
        name: name.to_string(),
        rss_kib: rss.parse().ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_lines() {
        assert_eq!(
            parse_ps("  1234 /srv/versions/pumpkin/0.2.0/pumpkin\n"),
            Some(ProcStatus {
                name: "pumpkin".into(),
                rss_kib: Some(1234)
            })
        );
        assert_eq!(parse_ps("88 java\n").map(|s| s.name), Some("java".into()));
        assert_eq!(parse_ps(""), None);
    }

    #[test]
    fn colours_are_stripped() {
        assert_eq!(strip_ansi("\u{1b}[32mINFO\u{1b}[0m ready"), "INFO ready");
    }
}
