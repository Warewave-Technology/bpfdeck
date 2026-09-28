mod app;
mod bpftrace;
mod catalog;
mod discovery;
mod headless;
mod list;
mod source;
mod sys;
mod ui;

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use ratatui::crossterm::event::{self, Event};

use app::App;

/// Browse, validate and run bpftrace scripts from a directory or git repository.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Local directory or git URL (https://… or git@…) containing .bt scripts.
    source: String,

    /// Path to the bpftrace binary.
    #[arg(long, default_value = "bpftrace")]
    bpftrace: String,

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
    let mut app = App::new(cli.source);

    // ratatui::init installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS)
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;
        // M0 only: polling loop. M3 replaces this with the Msg channel (docs/architecture.md).
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            app.on_key(key);
        }
    }
    Ok(())
}
