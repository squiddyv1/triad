//! The Rust dashboard's entry point: terminal setup and teardown, the event loop that
//! combines crossterm input with the poll and spinner timers, and the panic hook that
//! puts the shell back the way it was found.

mod app;
mod data;
mod signals;
mod theme;
mod ui;

use std::io::{self, stdout, Stdout};
use std::panic;
use std::time::{Duration, Instant};

use crossterm::cursor;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn main() -> io::Result<()> {
    let interval = parse_interval();
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, interval);
    restore_terminal(&mut terminal)?;
    result
}

fn run(terminal: &mut Tui, interval: Duration) -> io::Result<()> {
    let mut app = App::new(interval);
    // One frame before the first poll lands, so a slow CLI shows "loading…" rather than
    // an empty alternate screen.
    terminal.draw(|frame| ui::draw(frame, &app))?;

    // Verification hook, compiled out of release builds: with this set the app panics on
    // the first loop iteration so the panic hook's terminal restore can be exercised.
    #[cfg(debug_assertions)]
    if std::env::var_os("TRIAD_TUI_PANIC").is_some() {
        panic!("TRIAD_TUI_PANIC set: exercising the panic hook");
    }

    loop {
        let now = Instant::now();
        app.start_poll_if_due(now);
        let polled = app.pump(now);
        let ticked = app.tick(now);
        if polled || ticked {
            terminal.draw(|frame| ui::draw(frame, &app))?;
        }

        // The wait is bounded by the next timer, so an idle dashboard sleeps and a busy
        // one redraws only when the spinner or a poll actually moves.
        if event::poll(app.wait_hint(now))? {
            if let Event::Key(key) = event::read()? {
                app.on_key(key);
            }
        }

        if app.should_quit() || signals::terminated() {
            return Ok(());
        }
    }
}

fn setup_terminal() -> io::Result<Tui> {
    install_panic_hook();
    signals::install();
    enable_raw_mode()?;
    let mut out = stdout();
    if let Err(error) = execute!(out, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(error);
    }
    Terminal::new(CrosstermBackend::new(out))
}

fn restore_terminal(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}

/// If the app panics, leave the alternate screen and cooked mode before the default hook
/// prints the backtrace. Without this a crash leaves the shell unusable.
fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, cursor::Show);
        original(info);
    }));
}

/// `--interval N` (or `--interval=N`), the flag the launcher passes through, with the
/// same three-second default the Ink dashboard uses.
fn parse_interval() -> Duration {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = if arg == "--interval" {
            args.next()
        } else {
            arg.strip_prefix("--interval=").map(str::to_string)
        };
        if let Some(value) = value {
            if let Ok(seconds) = value.parse::<u64>() {
                // Clamped so a silly value cannot overflow `Instant + Duration` later.
                return Duration::from_secs(seconds.clamp(1, 3_600));
            }
        }
    }
    Duration::from_secs(3)
}
