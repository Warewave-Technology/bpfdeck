//! Debug entry points without the TUI: `--list` (discovery, metadata, validation) and
//! `--run <ID>` (stream parsed events to stdout). Useful on real hosts before/besides the
//! TUI; keep them working.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::bpftrace::command::{self, NamedArg, NamedValue, RunArgs};
use crate::bpftrace::runner::{self, Escalation, RunEvent, RunExit};
use crate::bpftrace::validate::{self, Strategy, ValidationRequest, Validator};
use crate::catalog::{self, Catalog, Script};
use crate::list::{self, Row};
use crate::model::describe::describe;
use crate::{bpftrace, source, sys};

/// Load the catalog and print its warnings to stderr.
fn load(input: &str) -> Result<Catalog> {
    let catalog = catalog::load(input, &source::default_cache_root()?)?;
    for warning in &catalog.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(catalog)
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().context("starting tokio runtime")
}

pub fn list(input: &str, bpftrace_path: &Path) -> Result<()> {
    let loaded = load(input)?;
    let resolved = &loaded.source;
    match &resolved.origin {
        source::Origin::Local => println!("source: local {}", resolved.root.display()),
        source::Origin::Git { spec, outcome } => println!(
            "source: git {}#{} ({outcome:?}) → {}",
            spec.url,
            spec.reference.as_deref().unwrap_or("HEAD"),
            resolved.root.display()
        ),
    }

    let validations = runtime()?.block_on(validate_all(bpftrace_path, &loaded.scripts));
    let rows: Vec<Row> = loaded
        .scripts
        .into_iter()
        .zip(validations)
        .map(|(script, validation)| Row {
            file: script.file,
            meta: script.meta,
            validation,
        })
        .collect();
    print!("{}", list::render(&rows));
    print!("{}", list::render_details(&rows));
    Ok(())
}

/// One result per script, in order; all `None` when bpftrace is not usable here.
async fn validate_all(bpftrace_path: &Path, scripts: &[Script]) -> Vec<Option<validate::Validation>> {
    let host = sys::detect();
    let info = match bpftrace::detect(bpftrace_path).await {
        Ok(info) => info,
        Err(e) => {
            eprintln!("warning: {e}; skipping validation");
            return vec![None; scripts.len()];
        }
    };
    let strategy = Strategy::choose(&info, host.privilege);
    println!(
        "bpftrace: {} ({}) · validation: {strategy:?} · privilege: {:?} · kernel: {} · lockdown: {:?}",
        info.version
            .map_or_else(|| "unknown version".to_string(), |v| v.to_string()),
        info.path.display(),
        host.privilege,
        host.kernel_release,
        host.lockdown,
    );
    if host.lockdown.blocks_bpftrace() {
        eprintln!("warning: kernel lockdown is active; bpftrace cannot load programs on this host");
    }
    let validator = Arc::new(Validator::new(
        &info,
        &host.kernel_release,
        strategy,
        validate::DEFAULT_WORKERS,
        validate::DEFAULT_TIMEOUT,
    ));
    let mut tasks = tokio::task::JoinSet::new();
    for (i, script) in scripts.iter().enumerate() {
        let req = ValidationRequest::new(&script.file.path, &script.content, &script.meta);
        let validator = validator.clone();
        tasks.spawn(async move { (i, validator.validate(&req).await) });
    }
    let mut out = vec![None; scripts.len()];
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((i, validation)) => out[i] = Some(validation),
            Err(e) => eprintln!("warning: validation task failed: {e}"),
        }
    }
    out
}

/// `--run ID [--param P …]`: `P` starting with `--` is a named param (`--x=v`, `--flag`),
/// anything else positional. Never `--unsafe` (D-009).
pub fn run(input: &str, bpftrace_path: &Path, id: &str, params: &[String]) -> Result<ExitCode> {
    let loaded = load(input)?;
    let file = &loaded
        .scripts
        .iter()
        .find(|s| s.file.id == id)
        .ok_or_else(|| anyhow!("no script with ID {id:?} (see --list)"))?
        .file;

    let mut positional = Vec::new();
    let mut named = Vec::new();
    for p in params {
        match p.strip_prefix("--") {
            Some(rest) => named.push(match rest.split_once('=') {
                Some((name, value)) => NamedArg {
                    name: name.into(),
                    value: NamedValue::Value(value.into()),
                },
                None => NamedArg {
                    name: rest.into(),
                    value: NamedValue::Flag(true),
                },
            }),
            None => positional.push(p.clone()),
        }
    }
    let args = RunArgs {
        script: &file.path,
        positional: &positional,
        named: &named,
        allow_unsafe: false,
    };
    let argv = command::run_argv(bpftrace_path, &args)?;
    let shown: Vec<_> = argv.iter().map(|a| a.to_string_lossy()).collect();
    eprintln!("$ {}", shown.join(" "));

    runtime()?.block_on(async {
        // Install handlers before spawning: a signal bpfdeck ignores (e.g. SIGINT when
        // started in the background) would otherwise be inherited as ignored by the child.
        let mut int = signal(SignalKind::interrupt())?;
        let mut term = signal(SignalKind::terminate())?;
        let mut hup = signal(SignalKind::hangup())?;
        let (tx, mut rx) = mpsc::channel(runner::CHANNEL_CAPACITY);
        let mut handle = runner::spawn(&argv, tx, Escalation::default())
            .with_context(|| format!("starting {}", bpftrace_path.display()))?;
        eprintln!(
            "started, process group {}; Ctrl-C stops (SIGINT to bpftrace)",
            handle.pgid()
        );
        let mut stopping = false;
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Some(RunEvent::Exited(exit)) => {
                        println!("{}", describe_exit(&exit));
                        return Ok(exit_code(&exit));
                    }
                    Some(RunEvent::Output(msg)) => println!("{}", describe(&msg)),
                    Some(RunEvent::Stderr(line)) => println!("stderr: {line}"),
                    None => bail!("runner stopped without an exit status"),
                },
                _ = int.recv(), if !stopping => {
                    eprintln!("stopping…");
                    handle.stop();
                    stopping = true;
                }
                _ = term.recv(), if !stopping => { handle.stop(); stopping = true; }
                _ = hup.recv(), if !stopping => { handle.stop(); stopping = true; }
            }
        }
    })
}

fn exit_code(exit: &RunExit) -> ExitCode {
    match exit.code {
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        None => ExitCode::FAILURE,
    }
}

fn describe_exit(exit: &RunExit) -> String {
    let mut s = match (exit.code, exit.signal) {
        (Some(code), _) => format!("exited: code {code}"),
        (None, Some(sig)) => format!("exited: killed by signal {sig}"),
        (None, None) => "exited: unknown status".to_string(),
    };
    if let Some(forced) = exit.forced {
        s.push_str(&format!(" (escalated to {forced})"));
    }
    if let Some(e) = &exit.error {
        s.push_str(&format!(" (error: {e})"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_descriptions() {
        let exit = |code, signal, forced| RunExit {
            code,
            signal,
            forced,
            error: None,
        };
        assert_eq!(describe_exit(&exit(Some(0), None, None)), "exited: code 0");
        assert_eq!(
            describe_exit(&exit(None, Some(9), Some(nix::sys::signal::Signal::SIGKILL))),
            "exited: killed by signal 9 (escalated to SIGKILL)"
        );
    }
}
