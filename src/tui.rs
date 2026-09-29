//! The TUI runtime: terminal setup, input thread, signal handling, the Msg/Cmd loop and
//! the executor that performs `Cmd`s (docs/architecture.md, "Concurrency model").

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::app::App;
use crate::app::target::TargetId;
use crate::bpftrace::coalesce::Coalescer;
use crate::bpftrace::runner::{self, Escalation, RunEvent, RunHandle};
use crate::bpftrace::validate::{self, Strategy, Validator};
use crate::model::log;
use crate::msg::{Cmd, Msg};
use crate::remote::Dest;
use crate::remote::connect::{self, Failure, SudoChoice};
use crate::remote::session::{Backend, SshTarget, Sudo};
use crate::{bpftrace, catalog, source, sys, ui};

const CHANNEL_CAPACITY: usize = 1024;
const INPUT_POLL: Duration = Duration::from_millis(50);
/// Redraw cadence while a run is active (elapsed time), spec §8.
const TICK: Duration = Duration::from_millis(250);
/// Longest the loop applies queued messages before drawing again. Keeps a printf flood
/// from starving the screen and the keyboard.
const FRAME_BUDGET: Duration = Duration::from_millis(30);

pub async fn run(input: String, bpftrace_path: PathBuf, export_dir: PathBuf, ssh: OsString) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    // Keys and signals get their own channel so they are never stuck behind run output.
    let (input_tx, mut input_rx) = mpsc::channel(CHANNEL_CAPACITY);
    // Handlers first, so nothing spawned later inherits a default/ignored disposition
    // we then change (see runner::spawn).
    spawn_signal_forwarder(input_tx.clone())?;

    // ratatui::init installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let input_gate = Arc::new(Mutex::new(()));
    let stop_input = Arc::new(AtomicBool::new(false));
    spawn_input_thread(input_tx, input_gate.clone(), stop_input.clone());

    let mut app = App::new(input);
    let mut exec = Executor {
        tx,
        bpftrace: bpftrace_path,
        validators: Arc::new(Mutex::new(HashMap::new())),
        input_gate,
        runs: HashMap::new(),
        export_dir,
        ssh,
        remotes: Arc::new(Mutex::new(HashMap::new())),
        connecting: HashMap::new(),
    };
    let result = event_loop(&mut terminal, &mut app, &mut exec, &mut input_rx, &mut rx).await;
    // Dropping a run handle kills a still running bpftrace (its whole process group; for a
    // remote run the local ssh, and EOF makes the runner stop bpftrace on the host).
    exec.shutdown().await;
    drop(exec);
    stop_input.store(true, Ordering::Release);
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    exec: &mut Executor,
    input_rx: &mut mpsc::Receiver<Msg>,
    rx: &mut mpsc::Receiver<Msg>,
) -> Result<()> {
    for cmd in app.init() {
        exec.execute(cmd, terminal)?;
    }
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;
        let msg = tokio::select! {
            biased;
            msg = input_rx.recv() => msg,
            msg = rx.recv() => msg,
        };
        let Some(msg) = msg else { break };
        let mut cmds = app.update(msg);
        // Apply what is already queued, keys first, within the frame budget.
        let deadline = Instant::now() + FRAME_BUDGET;
        while !app.should_quit && Instant::now() < deadline {
            if let Ok(msg) = input_rx.try_recv() {
                cmds.extend(app.update(msg));
                continue;
            }
            match rx.try_recv() {
                Ok(msg) => cmds.extend(app.update(msg)),
                Err(_) => break,
            }
        }
        for cmd in cmds {
            exec.execute(cmd, terminal)?;
        }
    }
    Ok(())
}

/// SIGTERM/SIGHUP (and SIGINT, which in raw mode can only come from `kill`) end bpfdeck.
/// Installing a SIGINT handler also means bpftrace never inherits an ignored SIGINT.
fn spawn_signal_forwarder(tx: mpsc::Sender<Msg>) -> Result<()> {
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
            _ = int.recv() => {}
        }
        let _ = tx.send(Msg::Terminate).await;
    });
    Ok(())
}

/// Blocking terminal reads on a dedicated thread. Each poll+read happens while holding
/// `gate`, so the editor handoff can take the gate and be sure no keystroke is stolen.
fn spawn_input_thread(tx: mpsc::Sender<Msg>, gate: Arc<Mutex<()>>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            let event = {
                let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
                match event::poll(INPUT_POLL) {
                    Ok(true) => event::read().ok(),
                    Ok(false) => None,
                    Err(_) => break,
                }
            };
            let msg = match event {
                Some(Event::Key(key)) => Msg::Key(key),
                Some(Event::Resize(..)) => Msg::Resize,
                _ => continue,
            };
            if tx.blocking_send(msg).is_err() {
                break;
            }
        }
    });
}

struct Executor {
    tx: mpsc::Sender<Msg>,
    bpftrace: PathBuf,
    /// Per target, set once bpftrace is detected there; `Cmd::Validate` only comes after.
    validators: Arc<Mutex<HashMap<TargetId, Arc<Validator>>>>,
    input_gate: Arc<Mutex<()>>,
    /// The current run per target (D-010). Replacing or dropping one kills its process
    /// group (for remote targets: the local `ssh`, which makes the runner stop bpftrace).
    runs: HashMap<TargetId, ActiveRun>,
    /// Where `w` writes exports (`--export-dir`).
    export_dir: PathBuf,
    /// The ssh binary (`--ssh`, for tests).
    ssh: OsString,
    /// Connected hosts; filled by the connect task before `Msg::Connected`.
    remotes: Arc<Mutex<HashMap<TargetId, Arc<SshTarget>>>>,
    /// Connect attempts in flight: the task and the target whose master it may open.
    connecting: HashMap<u64, (tokio::task::JoinHandle<()>, SshTarget)>,
}

struct ActiveRun {
    id: u64,
    // Field order matters: the handle (kill) drops before the spool file is removed.
    handle: RunHandle,
    spool: Option<PathBuf>,
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        if let Some(spool) = &self.spool {
            let _ = std::fs::remove_file(spool);
        }
    }
}

/// `<cache>/bpfdeck/runs/run-<pid>-<id>.ndjson` (temp dir as a fallback).
fn spool_path(run_id: u64) -> Option<PathBuf> {
    let dir = source::default_cache_root()
        .map(|c| c.join("runs"))
        .unwrap_or_else(|_| std::env::temp_dir());
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("run-{}-{run_id}.ndjson", std::process::id())))
}

/// Write `<stem>.txt` and, when a spool exists, `<stem>.ndjson` into `dir`.
fn export_files(
    dir: &Path,
    stem: &str,
    mut text: String,
    spool: Option<&Path>,
    truncated: bool,
) -> Result<Vec<PathBuf>> {
    let dir = std::fs::canonicalize(dir).with_context(|| format!("export dir {}", dir.display()))?;
    let mut written = Vec::new();
    if let Some(spool) = spool.filter(|s| s.exists()) {
        let ndjson = dir.join(format!("{stem}.ndjson"));
        std::fs::copy(spool, &ndjson).with_context(|| format!("writing {}", ndjson.display()))?;
        if truncated {
            text.push_str(&format!(
                "\nnote: the raw NDJSON copy stopped at {} MiB; later output is only in this text\n",
                runner::SPOOL_CAP / 1024 / 1024
            ));
        }
        written.push(ndjson);
    } else {
        text.push_str("\nnote: no raw NDJSON copy was recorded for this run\n");
    }
    let txt = dir.join(format!("{stem}.txt"));
    std::fs::write(&txt, text).with_context(|| format!("writing {}", txt.display()))?;
    written.insert(0, txt);
    Ok(written)
}

impl Executor {
    fn execute(&mut self, cmd: Cmd, terminal: &mut DefaultTerminal) -> Result<()> {
        match cmd {
            Cmd::Load { input } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = source::default_cache_root()
                        .map_err(anyhow::Error::from)
                        .and_then(|cache| catalog::load(&input, &cache))
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.blocking_send(Msg::Loaded(result));
                });
            }
            Cmd::Rescan { source } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = catalog::scan(source).map_err(|e| format!("{e:#}"));
                    let _ = tx.blocking_send(Msg::Loaded(result));
                });
            }
            Cmd::DetectEnv { target } => {
                let (tx, path, slot) = (self.tx.clone(), self.bpftrace.clone(), self.validators.clone());
                tokio::spawn(async move {
                    let host = sys::detect();
                    let bpftrace = match bpftrace::detect(&path).await {
                        Ok(info) => {
                            let strategy = Strategy::choose(&info, host.privilege);
                            let validator = Validator::new(
                                Backend::Local,
                                &info,
                                &host.kernel_release,
                                strategy,
                                validate::DEFAULT_WORKERS,
                                validate::DEFAULT_TIMEOUT,
                            );
                            lock(&slot).insert(target, Arc::new(validator));
                            Ok((info, strategy))
                        }
                        Err(e) => Err(e.to_string()),
                    };
                    let _ = tx
                        .send(Msg::EnvDetected {
                            target,
                            host,
                            bpftrace,
                        })
                        .await;
                });
            }
            Cmd::Validate { target, id, request } => {
                let Some(validator) = lock(&self.validators).get(&target).cloned() else {
                    return Ok(());
                };
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let validation = validator.validate(&request).await;
                    let _ = tx
                        .send(Msg::Validated {
                            target,
                            id,
                            content_hash: request.content_hash,
                            validation,
                        })
                        .await;
                });
            }
            Cmd::StartRun {
                target,
                run_id,
                argv,
                script,
            } => self.start_run(target, run_id, &argv, &script),
            Cmd::Connect {
                attempt,
                target,
                dest,
                sudo,
                bpftrace,
                interactive,
            } => self.connect(attempt, target, dest, sudo, bpftrace, interactive, terminal),
            Cmd::CancelConnect { attempt } => {
                if let Some((task, base)) = self.connecting.remove(&attempt) {
                    task.abort();
                    tokio::spawn(async move { connect::close_master(&base).await });
                }
            }
            Cmd::Disconnect { target } => {
                self.runs.remove(&target);
                lock(&self.validators).remove(&target);
                if let Some(remote) = lock(&self.remotes).remove(&target) {
                    tokio::spawn(async move { connect::close_master(&remote).await });
                }
            }
            Cmd::StopRun { target, run_id } => {
                if let Some(run) = self.runs.get_mut(&target).filter(|r| r.id == run_id) {
                    run.handle.stop();
                }
            }
            Cmd::ExportRun {
                run_id,
                script_id,
                host,
                text,
            } => {
                let active = self.runs.values().find(|r| r.id == run_id);
                let spool = active.and_then(|r| r.spool.clone());
                let truncated = active.is_some_and(|r| r.handle.spool_truncated());
                let (tx, dir) = (self.tx.clone(), self.export_dir.clone());
                tokio::task::spawn_blocking(move || {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let stem = crate::model::export::file_stem(host.as_deref(), &script_id, secs);
                    let result = export_files(&dir, &stem, text, spool.as_deref(), truncated)
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.blocking_send(Msg::Exported(result));
                });
            }
            Cmd::OpenEditor { id, path, copy } => {
                let result = tokio::task::block_in_place(|| self.edit(&path, copy, terminal));
                let msg = Msg::EditorClosed {
                    id,
                    copy,
                    result: result.map_err(|e| format!("{e:#}")),
                };
                // The loop drains the channel right after executing commands.
                let _ = self.tx.try_send(msg);
            }
        }
        Ok(())
    }

    /// Spawn bpftrace (locally, or on the host with `script` copied there) and forward
    /// its events (plus a tick for the elapsed time) as `Msg`s until it exits.
    fn start_run(&mut self, target: TargetId, run_id: u64, argv: &[OsString], script: &Path) {
        let tx = self.tx.clone();
        // The target's previous run is over (one at a time per target): drop it and its spool.
        self.runs.remove(&target);
        let (events_tx, events) = mpsc::channel(runner::CHANNEL_CAPACITY);
        let spool_file = spool_path(run_id);
        let spool = spool_file.clone().map(|path| runner::Spool {
            path,
            cap: runner::SPOOL_CAP,
        });
        let remote = lock(&self.remotes).get(&target).cloned();
        let started = match remote {
            Some(remote) => std::fs::read(script)
                .map_err(|e| format!("cannot read {}: {e}", script.display()))
                .and_then(|content| {
                    remote
                        .start_run(argv, &content, events_tx, Escalation::default(), spool)
                        .map_err(|e| format!("{}: {e}", remote.dest.label()))
                }),
            None => runner::spawn(argv, events_tx, Escalation::default(), spool.as_ref()).map_err(|e| {
                let program = argv
                    .first()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                format!("cannot run {program}: {e}")
            }),
        };
        let handle = match started {
            Ok(handle) => handle,
            Err(reason) => {
                tokio::spawn(async move {
                    let _ = tx.send(Msg::RunFailed { run_id, reason }).await;
                });
                return;
            }
        };
        self.runs.insert(
            target,
            ActiveRun {
                id: run_id,
                handle,
                spool: spool_file,
            },
        );
        tokio::spawn(forward_run(run_id, events, tx, TICK));
    }

    /// Run the connect checks in the background. `interactive`: first open the SSH master
    /// in the plain terminal (TUI suspended, like the editor), so SSH can ask for a
    /// passphrase, password or host key confirmation itself; bpfdeck never sees them.
    #[allow(clippy::too_many_arguments)]
    fn connect(
        &mut self,
        attempt: u64,
        target: TargetId,
        dest: Dest,
        sudo: SudoChoice,
        bpftrace: Option<String>,
        interactive: bool,
        terminal: &mut DefaultTerminal,
    ) {
        let tx = self.tx.clone();
        let dir = match connect::control_dir() {
            Ok(dir) => dir,
            Err(e) => {
                let _ = tx.try_send(Msg::ConnectFailed {
                    attempt,
                    reason: Some(e),
                });
                return;
            }
        };
        let base = SshTarget {
            dest: dest.clone(),
            ssh: self.ssh.clone(),
            control_path: dir.join("%C"),
            sudo: Sudo::None,
        };
        let log = connect::master_log(&dir, attempt);
        if interactive {
            let opened = tokio::task::block_in_place(|| self.master_in_terminal(&base, &log, terminal));
            if let Err(e) = opened {
                let _ = tx.try_send(Msg::ConnectFailed {
                    attempt,
                    reason: Some(format!("{e:#}")),
                });
                return;
            }
        }
        self.connecting.retain(|_, (task, _)| !task.is_finished());
        let (remotes, validators) = (self.remotes.clone(), self.validators.clone());
        let task_base = base.clone();
        let task = tokio::spawn(async move {
            // Checks go out in order, before the final message.
            let (report, mut checks) = mpsc::channel(16);
            let forward_tx = tx.clone();
            let forward = tokio::spawn(async move {
                while let Some(check) = checks.recv().await {
                    let _ = forward_tx.send(Msg::ConnectCheck { attempt, check }).await;
                }
            });
            let result = connect::connect(task_base, sudo, bpftrace, &log, interactive, &report).await;
            drop(report);
            let _ = forward.await;
            let msg = match result {
                Ok(c) => {
                    let strategy = Strategy::choose(&c.bpftrace, c.system.privilege);
                    let validator = Validator::new(
                        Backend::Ssh(c.target.clone()),
                        &c.bpftrace,
                        &c.system.kernel_release,
                        strategy,
                        validate::DEFAULT_WORKERS,
                        validate::DEFAULT_TIMEOUT,
                    );
                    lock(&validators).insert(target, Arc::new(validator));
                    lock(&remotes).insert(target, c.target.clone());
                    tokio::spawn(watch_master(
                        target,
                        c.target,
                        remotes.clone(),
                        validators.clone(),
                        tx.clone(),
                        WATCH_EVERY,
                    ));
                    Msg::Connected {
                        attempt,
                        target,
                        dest,
                        info: c.info,
                        host: c.system,
                        bpftrace: (c.bpftrace, strategy),
                    }
                }
                Err(Failure::NeedsAuth(message)) => Msg::ConnectNeedsAuth { attempt, message },
                Err(Failure::Failed) => Msg::ConnectFailed {
                    attempt,
                    reason: None,
                },
            };
            let _ = tx.send(msg).await;
        });
        self.connecting.insert(attempt, (task, base));
    }

    /// `ssh -fN` without BatchMode, in the plain terminal.
    fn master_in_terminal(&self, base: &SshTarget, log: &Path, terminal: &mut DefaultTerminal) -> Result<()> {
        let argv = base.master_argv(false, log);
        let _input_paused = self.input_gate.lock().unwrap_or_else(|e| e.into_inner());
        ratatui::restore();
        println!(
            "bpfdeck: connecting to {}. ssh may ask for a passphrase, password or host key confirmation.",
            base.dest.label()
        );
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
        let resumed = resume(terminal);
        let message = std::fs::read_to_string(log).unwrap_or_default();
        let _ = std::fs::remove_file(log);
        resumed?;
        let status = status.with_context(|| format!("cannot run {}", base.ssh.to_string_lossy()))?;
        if !status.success() {
            let last = message
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default();
            anyhow::bail!(
                "ssh exited with {status}{}{last}",
                if last.is_empty() { "" } else { ": " }
            );
        }
        Ok(())
    }

    /// Stop runs and close every SSH master (quitting).
    async fn shutdown(&mut self) {
        self.runs.clear();
        for (task, _) in self.connecting.values() {
            task.abort();
        }
        let mut masters: Vec<SshTarget> = lock(&self.remotes).drain().map(|(_, r)| (*r).clone()).collect();
        masters.extend(self.connecting.drain().map(|(_, (_, base))| base));
        let mut closing = tokio::task::JoinSet::new();
        for master in masters {
            closing.spawn(async move { connect::close_master(&master).await });
        }
        closing.join_all().await;
    }

    /// Suspend the TUI, run `$VISUAL`/`$EDITOR` (split on whitespace, no shell), resume.
    /// With `copy`, the editor gets a temporary copy that is deleted afterwards.
    fn edit(&self, path: &Path, copy: bool, terminal: &mut DefaultTerminal) -> Result<()> {
        let editor = std::env::var("VISUAL")
            .ok()
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| "vi".to_string());
        let mut argv = editor.split_whitespace();
        let program = argv.next().unwrap_or("vi").to_string();
        let args: Vec<String> = argv.map(String::from).collect();

        let target = if copy {
            temp_copy(path)?
        } else {
            path.to_path_buf()
        };
        let _input_paused = self.input_gate.lock().unwrap_or_else(|e| e.into_inner());
        ratatui::restore();
        let status = Command::new(&program)
            .args(&args)
            .arg(&target)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
        let resumed = resume(terminal);
        if copy {
            let _ = std::fs::remove_file(&target);
        }
        resumed?;
        let status = status.with_context(|| format!("cannot run {program}"))?;
        if !status.success() {
            anyhow::bail!("{program} exited with {status}");
        }
        Ok(())
    }
}

fn resume(terminal: &mut DefaultTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()
}

/// Forward a run's events to the app until it exits, plus a tick for the elapsed time.
/// Events are held in a [`Coalescer`] until the app channel has room: when the UI keeps
/// up every batch is tiny; when it lags, snapshots collapse to the latest per map and
/// excess text lines are dropped and counted (spec §6.5).
async fn forward_run(
    run_id: u64,
    mut events: mpsc::Receiver<RunEvent>,
    tx: mpsc::Sender<Msg>,
    tick_every: Duration,
) {
    if tx
        .send(Msg::RunStarted {
            run_id,
            at: Instant::now(),
        })
        .await
        .is_err()
    {
        return;
    }
    let mut pending = Coalescer::new(log::DEFAULT_CAPACITY);
    let mut tick = tokio::time::interval(tick_every);
    let mut reading = true;
    while reading || !pending.is_empty() {
        tokio::select! {
            event = events.recv(), if reading => match event {
                Some(event) => {
                    pending.push(event);
                    // Take what is already there without yielding (bounded by the channel).
                    for _ in 0..runner::CHANNEL_CAPACITY {
                        let Ok(event) = events.try_recv() else { break };
                        pending.push(event);
                    }
                }
                // The runner closes the channel right after `Exited`.
                None => reading = false,
            },
            permit = tx.reserve(), if !pending.is_empty() => match permit {
                Ok(permit) => permit.send(Msg::Run { run_id, at: Instant::now(), batch: pending.take() }),
                Err(_) => return,
            },
            _ = tick.tick(), if reading => {
                // Only a redraw hint: skip it rather than wait when the app is busy.
                let _ = tx.try_send(Msg::Tick(Instant::now()));
            }
        }
    }
}

/// How often a connected target's SSH master is checked.
const WATCH_EVERY: Duration = Duration::from_secs(10);

/// Check the target's master until it is disconnected (no longer in `remotes`) or gone;
/// then forget it and tell the app. Runs over it end by themselves (ssh exits 255).
async fn watch_master(
    target: TargetId,
    remote: Arc<SshTarget>,
    remotes: Arc<Mutex<HashMap<TargetId, Arc<SshTarget>>>>,
    validators: Arc<Mutex<HashMap<TargetId, Arc<Validator>>>>,
    tx: mpsc::Sender<Msg>,
    every: Duration,
) {
    let current = || {
        lock(&remotes)
            .get(&target)
            .is_some_and(|r| Arc::ptr_eq(r, &remote))
    };
    loop {
        tokio::time::sleep(every).await;
        if !current() {
            return;
        }
        if connect::master_alive(&remote).await || !current() {
            continue;
        }
        lock(&remotes).remove(&target);
        lock(&validators).remove(&target);
        let reason = "the ssh connection closed".to_string();
        let _ = tx.send(Msg::ConnectionLost { target, reason }).await;
        return;
    }
}

/// `$TMPDIR/bpfdeck-<pid>-<name>`, keeping the file name so editors pick the right mode.
fn temp_copy(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "script.bt".into());
    let target = std::env::temp_dir().join(format!("bpfdeck-{}-{name}", std::process::id()));
    std::fs::copy(path, &target).with_context(|| format!("copying {} for editing", path.display()))?;
    Ok(target)
}

/// A poisoned lock only means another task panicked mid-insert; the map is still usable.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::command::{self, RunArgs};
    use crate::bpftrace::json::{MapValue, OutputMsg};
    use crate::bpftrace::testutil::{fake, script};
    use crate::remote::session::testutil;

    #[tokio::test]
    async fn a_dead_master_is_reported_once_and_forgotten() {
        let (tx, mut rx) = mpsc::channel(8);
        let remotes = Arc::new(Mutex::new(HashMap::new()));
        let validators = Arc::new(Mutex::new(HashMap::new()));
        let every = Duration::from_millis(20);

        let alive = Arc::new(testutil::target("db-02", Sudo::None));
        lock(&remotes).insert(1, alive.clone());
        let watcher = tokio::spawn(watch_master(
            1,
            alive,
            remotes.clone(),
            validators.clone(),
            tx.clone(),
            every,
        ));
        tokio::time::sleep(every * 5).await;
        assert!(!watcher.is_finished(), "a live master keeps being watched");
        // Disconnecting ends the watch without a message.
        lock(&remotes).remove(&1);
        tokio::time::timeout(Duration::from_secs(5), watcher)
            .await
            .expect("ends")
            .expect("join");

        let dropped = Arc::new(testutil::target("dropped-2", Sudo::None));
        lock(&remotes).insert(2, dropped.clone());
        tokio::spawn(watch_master(2, dropped, remotes.clone(), validators, tx, every));
        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("message");
        assert!(
            matches!(msg, Some(Msg::ConnectionLost { target: 2, .. })),
            "{msg:?}"
        );
        assert!(lock(&remotes).is_empty());
        assert!(rx.recv().await.is_none(), "reported once");
    }

    /// A printf/map flood against a deliberately slow consumer: nothing is lost silently,
    /// snapshots collapse, and the exit (after the last snapshot) always arrives.
    #[test]
    fn export_writes_text_and_raw_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = dir.path().join("spool.ndjson");
        std::fs::write(&spool, "{\"type\": \"printf\", \"data\": \"x\"}\n").expect("spool");
        let paths =
            export_files(dir.path(), "bpfdeck-x-1", "report\n".into(), Some(&spool), false).expect("export");
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("bpfdeck-x-1.txt") && paths[1].ends_with("bpfdeck-x-1.ndjson"));
        assert_eq!(std::fs::read_to_string(&paths[0]).expect("txt"), "report\n");
        assert_eq!(
            std::fs::read(&paths[1]).expect("ndjson"),
            std::fs::read(&spool).expect("spool")
        );

        let paths =
            export_files(dir.path(), "bpfdeck-x-2", "report\n".into(), Some(&spool), true).expect("export");
        assert!(
            std::fs::read_to_string(&paths[0])
                .expect("txt")
                .contains("stopped at 256 MiB")
        );

        let paths = export_files(dir.path(), "bpfdeck-x-3", "report\n".into(), None, false).expect("export");
        assert_eq!(paths.len(), 1);
        assert!(
            std::fs::read_to_string(&paths[0])
                .expect("txt")
                .contains("no raw NDJSON copy")
        );

        assert!(export_files(&dir.path().join("missing"), "x", String::new(), None, false).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flood_is_coalesced_and_accounted_for() {
        const LINES: u64 = 100_000;
        const MAPS: u64 = 50_000;
        let dir = tempfile::tempdir().expect("tempdir");
        let s = script(
            &dir,
            "flood.bt",
            &format!("// fake: flood={LINES}\n// fake: flood_maps={MAPS}\n// fake: exit=0\nBEGIN {{}}\n"),
        );
        let args = RunArgs {
            script: &s,
            positional: &[],
            named: &[],
            allow_unsafe: false,
        };
        let argv = command::run_argv(&fake(), &args).expect("argv");
        let (events_tx, events) = mpsc::channel(runner::CHANNEL_CAPACITY);
        let _handle = runner::spawn(&argv, events_tx, Escalation::default(), None).expect("spawn");
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(forward_run(1, events, tx, Duration::from_millis(50)));

        let (mut texts, mut dropped, mut maps, mut batches) = (0u64, 0u64, 0u64, 0u64);
        let mut last_map = None;
        let mut exited = false;
        while !exited {
            let msg = tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .expect("flood did not finish in time")
                .expect("forwarder ended without Exited");
            let Msg::Run { batch, .. } = msg else { continue };
            batches += 1;
            dropped += batch.dropped;
            let n = batch.events.len();
            for (i, event) in batch.events.into_iter().enumerate() {
                match event {
                    RunEvent::Output(OutputMsg::Text { .. }) => texts += 1,
                    RunEvent::Output(OutputMsg::Map {
                        value: MapValue::Keyed(rows),
                        ..
                    }) => {
                        maps += 1;
                        last_map = rows.first().map(|(_, v)| v.clone());
                    }
                    RunEvent::Exited(exit) => {
                        assert_eq!(exit.code, Some(0));
                        assert_eq!(i, n - 1, "exit is the last event");
                        exited = true;
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
            // A slow UI: 1 ms per message.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(
            texts + dropped,
            LINES,
            "every printf line delivered or counted as dropped"
        );
        assert!(maps < MAPS, "snapshots were coalesced ({maps} of {MAPS})");
        assert_eq!(
            last_map,
            Some(serde_json::json!(MAPS)),
            "the latest snapshot survives"
        );
        assert!(batches < LINES, "events were batched ({batches} messages)");
    }
}
