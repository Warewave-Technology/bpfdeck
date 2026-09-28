//! The TUI runtime: terminal setup, input thread, signal handling, the Msg/Cmd loop and
//! the executor that performs `Cmd`s (docs/architecture.md, "Concurrency model").

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::app::App;
use crate::bpftrace::validate::{self, Strategy, Validator};
use crate::msg::{Cmd, Msg};
use crate::{bpftrace, catalog, source, sys, ui};

const CHANNEL_CAPACITY: usize = 1024;
const INPUT_POLL: Duration = Duration::from_millis(50);

pub async fn run(input: String, bpftrace_path: PathBuf) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    // Handlers first, so nothing spawned later inherits a default/ignored disposition
    // we then change (see runner::spawn).
    spawn_signal_forwarder(tx.clone())?;

    // ratatui::init installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let input_gate = Arc::new(Mutex::new(()));
    let stop_input = Arc::new(AtomicBool::new(false));
    spawn_input_thread(tx.clone(), input_gate.clone(), stop_input.clone());

    let mut app = App::new(input);
    let mut exec = Executor {
        tx,
        bpftrace: bpftrace_path,
        validator: Arc::new(OnceLock::new()),
        input_gate,
    };
    let result = event_loop(&mut terminal, &mut app, &mut exec, &mut rx).await;
    stop_input.store(true, Ordering::Release);
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    exec: &mut Executor,
    rx: &mut mpsc::Receiver<Msg>,
) -> Result<()> {
    for cmd in app.init() {
        exec.execute(cmd, terminal)?;
    }
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;
        let Some(msg) = rx.recv().await else { break };
        let mut cmds = app.update(msg);
        // Apply everything that is already queued before drawing again.
        while let Ok(msg) = rx.try_recv() {
            cmds.extend(app.update(msg));
        }
        for cmd in cmds {
            exec.execute(cmd, terminal)?;
        }
    }
    Ok(())
}

fn spawn_signal_forwarder(tx: mpsc::Sender<Msg>) -> Result<()> {
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
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
