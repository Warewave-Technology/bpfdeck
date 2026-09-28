mod app;
mod discovery;
mod source;
mod ui;

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut app = App::new(cli.source);

    // ratatui::init installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
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
