mod app;
mod bpftrace;
mod catalog;
mod discovery;
mod headless;
mod keymap;
mod list;
mod model;
mod msg;
mod source;
mod sys;
mod tui;
mod ui;

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

/// Browse, validate and run bpftrace scripts from a directory or git repository.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Local directory or git URL (https://… or git@…) containing .bt scripts.
    source: String,

    /// Path to the bpftrace binary.
    #[arg(long, default_value = "bpftrace")]
    bpftrace: String,

    /// Directory where `w` in the run view writes exports (.txt report + raw .ndjson).
    #[arg(long, default_value = ".")]
    export_dir: std::path::PathBuf,

    /// Print discovered scripts, their metadata and validation status, then exit (debug aid).
    #[arg(long, conflicts_with = "run")]
    list: bool,

    /// Run the script with this ID without the TUI, printing parsed output (debug aid).
    #[arg(long, value_name = "ID")]
    run: Option<String>,

    /// Parameter for --run, repeatable: `--param=--name=value` / `--param=--flag` for
    /// named, anything else positional.
    #[arg(
        long = "param",
        value_name = "ARG",
        allow_hyphen_values = true,
        requires = "run"
    )]
    params: Vec<String>,
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let bpftrace = Path::new(&cli.bpftrace);
    if cli.list {
        headless::list(&cli.source, bpftrace)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(id) = &cli.run {
        return headless::run(&cli.source, bpftrace, id, &cli.params);
    }
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(tui::run(cli.source, bpftrace.to_path_buf(), cli.export_dir))?;
    Ok(ExitCode::SUCCESS)
}
