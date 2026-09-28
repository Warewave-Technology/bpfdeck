//! The TUI runtime: terminal setup, input thread, signal handling, the Msg/Cmd loop and
//! the executor that performs `Cmd`s (docs/architecture.md, "Concurrency model").

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::app::App;
use crate::bpftrace::coalesce::Coalescer;
use crate::bpftrace::runner::{self, Escalation, RunEvent, RunHandle};
use crate::bpftrace::validate::{self, Strategy, Validator};
use crate::model::log;
use crate::msg::{Cmd, Msg};
use crate::{bpftrace, catalog, source, sys, ui};

const CHANNEL_CAPACITY: usize = 1024;
const INPUT_POLL: Duration = Duration::from_millis(50);
/// Redraw cadence while a run is active (elapsed time), spec §8.
const TICK: Duration = Duration::from_millis(250);
/// Longest the loop applies queued messages before drawing again. Keeps a printf flood
/// from starving the screen and the keyboard.
const FRAME_BUDGET: Duration = Duration::from_millis(30);

pub async fn run(input: String, bpftrace_path: PathBuf) -> Result<()> {
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
        validator: Arc::new(OnceLock::new()),
        input_gate,
        run: None,
    };
    let result = event_loop(&mut terminal, &mut app, &mut exec, &mut input_rx, &mut rx).await;
    // Dropping the handle kills a still running bpftrace (its whole process group).
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
    /// Set once bpftrace is detected; `Cmd::Validate` only arrives after that.
    validator: Arc<OnceLock<Arc<Validator>>>,
    input_gate: Arc<Mutex<()>>,
    /// The current run (D-010). Replacing or dropping it kills a live process group.
    run: Option<(u64, RunHandle)>,
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
            Cmd::DetectEnv => {
                let (tx, path, slot) = (self.tx.clone(), self.bpftrace.clone(), self.validator.clone());
                tokio::spawn(async move {
                    let host = sys::detect();
                    let bpftrace = match bpftrace::detect(&path).await {
                        Ok(info) => {
                            let strategy = Strategy::choose(&info, host.privilege);
                            let validator = Validator::new(
                                &info,
                                &host.kernel_release,
                                strategy,
                                validate::DEFAULT_WORKERS,
                                validate::DEFAULT_TIMEOUT,
                            );
                            let _ = slot.set(Arc::new(validator));
                            Ok((info, strategy))
                        }
                        Err(e) => Err(e.to_string()),
                    };
                    let _ = tx.send(Msg::EnvDetected { host, bpftrace }).await;
                });
            }
            Cmd::Validate { id, request } => {
                let Some(validator) = self.validator.get().cloned() else {
                    return Ok(());
                };
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let validation = validator.validate(&request).await;
                    let _ = tx
                        .send(Msg::Validated {
                            id,
                            content_hash: request.content_hash,
                            validation,
                        })
                        .await;
                });
            }
            Cmd::StartRun { run_id, argv } => self.start_run(run_id, &argv),
            Cmd::StopRun { run_id } => {
                if let Some((id, handle)) = &mut self.run
                    && *id == run_id
                {
                    handle.stop();
                }
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

    /// Spawn bpftrace and forward its events (plus a tick for the elapsed time) as `Msg`s
    /// until it exits.
    fn start_run(&mut self, run_id: u64, argv: &[std::ffi::OsString]) {
        let tx = self.tx.clone();
        let (events_tx, events) = mpsc::channel(runner::CHANNEL_CAPACITY);
        let handle = match runner::spawn(argv, events_tx, Escalation::default()) {
            Ok(handle) => handle,
            Err(e) => {
                let program = argv
                    .first()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let reason = format!("cannot run {program}: {e}");
                tokio::spawn(async move {
                    let _ = tx.send(Msg::RunFailed { run_id, reason }).await;
                });
                return;
            }
        };
        self.run = Some((run_id, handle));
        tokio::spawn(forward_run(run_id, events, tx, TICK));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::command::{self, RunArgs};
    use crate::bpftrace::json::{MapValue, OutputMsg};
    use crate::bpftrace::testutil::{fake, script};

    /// A printf/map flood against a deliberately slow consumer: nothing is lost silently,
    /// snapshots collapse, and the exit (after the last snapshot) always arrives.
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
        let _handle = runner::spawn(&argv, events_tx, Escalation::default()).expect("spawn");
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
