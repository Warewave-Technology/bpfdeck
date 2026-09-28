mod app;
mod discovery;
mod list;
mod source;
mod ui;

use std::time::Duration;

use anyhow::{Context, Result};
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

    /// Print discovered scripts and their metadata as a table, then exit (debug aid).
    #[arg(long)]
    list: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.list {
        return print_list(&cli.source);
    }
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

fn print_list(input: &str) -> Result<()> {
    let cache_root = source::default_cache_root()?;
    let resolved = source::resolve(input, &cache_root).with_context(|| format!("resolving {input}"))?;
    let found = match &resolved.file {
        Some(file) => discovery::single_file(file)?,
        None => discovery::walk(&resolved.root)?,
    };
    for warning in resolved.warnings.iter().chain(&found.warnings) {
        eprintln!("warning: {warning}");
    }

    let mut scripts = Vec::with_capacity(found.scripts.len());
    for file in found.scripts {
        let src = match std::fs::read(&file.path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                eprintln!("warning: cannot read {}: {e}", file.id);
                continue;
            }
        };
        let meta = discovery::metadata::extract(&src);
        scripts.push((file, meta));
    }
    match &resolved.origin {
        source::Origin::Local => println!("source: local {}", resolved.root.display()),
        source::Origin::Git { spec, outcome } => println!(
            "source: git {}#{} ({outcome:?}) → {}",
            spec.url,
            spec.reference.as_deref().unwrap_or("HEAD"),
            resolved.root.display()
        ),
    }
    print!("{}", list::render(&scripts));
    Ok(())
}
