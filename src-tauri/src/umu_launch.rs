//! Runs umu-run with its output in a per-game log: the port of `Umu.launchExecutable`
//! (src/main/services/umu.ts) and src/main/services/umu-launch-monitor.ts.
//! A umu-run that exits within the quick-exit window is a failed launch; a later exit is
//! checked against the log, and the child is always waited on so it never stays defunct.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::launch::LaunchCommand;
use crate::umu_monitor::{setup_failure_message, UmuEvent, UmuOutputMonitor};

pub const QUICK_EXIT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(500);
const MAX_READ: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UmuStatus {
    Progress(Option<String>),
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarlyExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub failure: Option<String>,
}

impl std::fmt::Display for EarlyExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = |v: Option<i32>| v.map_or("null".to_string(), |v| v.to_string());
        write!(f, "umu-run exited early with code={} signal={}", n(self.code), n(self.signal))?;
        if let Some(failure) = &self.failure {
            write!(f, ": {failure}")?;
        }
        Ok(())
    }
}

/// `logs/umu-<key>.log`, with the key made safe for a file name.
pub fn log_path(logs_dir: &Path, key: &str) -> PathBuf {
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' }).collect();
    logs_dir.join(format!("umu-{safe}.log"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The `ENV=... cmd args` line written above each run, so the log shows what was started.
pub fn command_line(launch: &LaunchCommand) -> String {
    let env = launch.env.iter().map(|(k, v)| format!("{k}={}", shell_quote(v)));
    let cmd = std::iter::once(&launch.command).chain(&launch.args).map(|a| shell_quote(a));
    env.chain(cmd).collect::<Vec<_>>().join(" ")
}

/// Reads what was appended to the log since `offset`, never splitting a UTF-8 character.
struct LogTail {
    path: PathBuf,
    offset: u64,
    pending: Vec<u8>,
}

impl LogTail {
    fn read(&mut self) -> String {
        let Ok(mut file) = std::fs::File::open(&self.path) else { return String::new() };
        let mut out = String::new();
        loop {
            let mut chunk = Vec::new();
            let read = file.seek(SeekFrom::Start(self.offset)).and_then(|_| (&mut file).take(MAX_READ).read_to_end(&mut chunk));
            match read {
                Ok(n) if n > 0 => {
                    self.offset += n as u64;
                    self.pending.extend_from_slice(&chunk);
                    out.push_str(&self.take_valid());
                }
                _ => return out,
            }
        }
    }

    fn take_valid(&mut self) -> String {
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            // An invalid byte (not a cut-off character) is replaced rather than kept forever.
            Err(e) if e.error_len().is_some() => self.pending.len(),
            Err(e) => e.valid_up_to(),
        };
        let bytes: Vec<u8> = self.pending.drain(..valid).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// A started umu-run whose first seconds are still being watched.
pub struct Pending(mpsc::Receiver<Result<u32, EarlyExit>>);

impl Pending {
    /// Blocks until umu-run has lived through the quick-exit window, or exited inside it.
    pub fn wait(self) -> Result<u32, EarlyExit> {
        self.0.recv().unwrap_or(Err(EarlyExit { code: None, signal: None, failure: Some("umu-run monitor stopped".into()) }))
    }
}

pub struct Hooks {
    pub on_status: Box<dyn Fn(UmuStatus) + Send>,
    pub game_detected: Box<dyn Fn() -> bool + Send>,
    pub on_exit: Box<dyn FnOnce(Option<i32>) + Send>,
}

/// Starts `launch` with stdout and stderr appended to `log`, then watches it on its own thread.
pub fn launch(launch: &LaunchCommand, cwd: &Path, log: &Path, quick_exit: Duration, hooks: Hooks) -> std::io::Result<Pending> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(log)?;
    writeln!(file, "\n[{}] Launching with umu-run\nCommand: {}", crate::library::iso_now(), command_line(launch))?;
    let start = file.metadata()?.len();
    let mut command = Command::new(&launch.command);
    crate::proton::use_host_env(&mut command)
        .args(&launch.args)
        .envs(launch.env.iter().cloned())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file.try_clone()?);
    #[cfg(unix)]
    {
        // Its own process group, as Hydra's detached spawn: closing Chimaera leaves the game running.
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = writeln!(file, "[{}] Failed to spawn umu-run ({}): {e}", crate::library::iso_now(), launch.command);
            return Err(e);
        }
    };
    let pid = child.id();
    let log = log.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Hooks { on_status, game_detected, on_exit } = hooks;
        let mut monitor = UmuOutputMonitor::default();
        let mut tail = LogTail { path: log.clone(), offset: start, pending: Vec::new() };
        let progress = |events: Vec<UmuEvent>| {
            for event in events {
                if let UmuEvent::Progress(message) = event {
                    if !game_detected() {
                        on_status(UmuStatus::Progress(Some(message)));
                    }
                }
            }
        };
        on_status(UmuStatus::Progress(None));
        let started = Instant::now();
        let mut survived = false;
        let status = loop {
            progress(monitor.feed(&tail.read()));
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(_) => break child.wait().ok(),
            }
            if !survived && started.elapsed() >= quick_exit {
                survived = true;
                let _ = tx.send(Ok(pid));
            }
            std::thread::sleep(POLL.min(quick_exit));
        };
        progress(monitor.feed(&tail.read()));
        progress(monitor.flush());
        let code = status.and_then(|s| s.code());
        #[cfg(unix)]
        let signal = status.and_then(|s| std::os::unix::process::ExitStatusExt::signal(&s));
        #[cfg(not(unix))]
        let signal = None;
        if !survived {
            let early = EarlyExit { code, signal, failure: monitor.failure_message().map(str::to_string) };
            if let Ok(mut file) = OpenOptions::new().append(true).open(&log) {
                let _ = writeln!(file, "[{}] {early}", crate::library::iso_now());
            }
            let _ = tx.send(Err(early));
        } else {
            match setup_failure_message(code, signal, monitor.failure_message(), monitor.has_fatal_error(), game_detected()) {
                Some(message) => on_status(UmuStatus::Failed(message)),
                None => on_status(UmuStatus::Ready),
            }
        }
        on_exit(code);
    });
    Ok(Pending(rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn run(script: &str, quick_exit: Duration) -> (Result<u32, EarlyExit>, Vec<UmuStatus>, String) {
        let dir = std::env::temp_dir().join(format!("chimaera-umu-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = log_path(&dir, "steam:1");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (done_tx, done_rx) = mpsc::channel();
        let s = seen.clone();
        let hooks = Hooks {
            on_status: Box::new(move |st| s.lock().unwrap().push(st)),
            game_detected: Box::new(|| false),
            on_exit: Box::new(move |_| done_tx.send(()).unwrap()),
        };
        let cmd = LaunchCommand { command: "sh".into(), args: vec!["-c".into(), script.into()], env: vec![("A".into(), "it's".into())] };
        let result = launch(&cmd, &dir, &log, quick_exit, hooks).unwrap().wait();
        done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let statuses = seen.lock().unwrap().clone();
        let _ = std::fs::remove_dir_all(&dir);
        (result, statuses, text)
    }

    #[test]
    fn early_exit_carries_the_failure() {
        let (result, _, log) = run("echo 'ERROR: no runtime' >&2; exit 3", Duration::from_secs(2));
        assert_eq!(result, Err(EarlyExit { code: Some(3), signal: None, failure: Some("no runtime".into()) }));
        assert!(log.contains("Command: A='it'\\''s' 'sh' '-c'"), "{log}");
        assert!(log.contains("umu-run exited early with code=3 signal=null: no runtime"), "{log}");
    }

    #[test]
    fn late_failure_and_progress_are_reported() {
        let (result, statuses, _) = run("echo 'INFO: Downloading steamrt4, please wait...'; sleep 1; echo 'RuntimeError: boom'; exit 1", Duration::from_millis(300));
        assert!(result.is_ok());
        assert_eq!(statuses.first(), Some(&UmuStatus::Progress(None)));
        assert!(statuses.contains(&UmuStatus::Progress(Some("Downloading steamrt4".into()))), "{statuses:?}");
        assert_eq!(statuses.last(), Some(&UmuStatus::Failed("RuntimeError: boom".into())));
    }

    #[test]
    fn clean_exit_is_ready() {
        let (result, statuses, _) = run("sleep 1", Duration::from_millis(300));
        assert!(result.is_ok());
        assert_eq!(statuses.last(), Some(&UmuStatus::Ready));
    }

    #[test]
    fn tail_keeps_split_characters_whole() {
        let mut tail = LogTail { path: PathBuf::new(), offset: 0, pending: "é".as_bytes()[..1].to_vec() };
        assert_eq!(tail.take_valid(), "");
        tail.pending.push("é".as_bytes()[1]);
        assert_eq!(tail.take_valid(), "é");
    }
}
