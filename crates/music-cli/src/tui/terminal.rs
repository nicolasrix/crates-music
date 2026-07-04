//! Raw-mode + alternate-screen lifecycle. The invariants:
//!
//! 1. `restore()` is idempotent — the panic hook may fire before the guard's
//!    `Drop`, and both may run.
//! 2. The panic hook restores the terminal *before* the default hook prints,
//!    so panic messages land on a usable screen instead of a raw-mode
//!    alternate buffer the shell can't recover from.

use std::io::stdout;

use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

#[derive(Debug)]
pub(crate) struct TerminalGuard;

impl TerminalGuard {
    pub(crate) fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Leave the alternate screen and raw mode. Safe to call repeatedly; errors
/// are ignored (we're on the way out — there's nowhere to report them).
pub(crate) fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen);
}

/// Chain a terminal restore in front of the existing panic hook.
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
