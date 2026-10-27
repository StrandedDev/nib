use std::io::{self, Write};

use anyhow::Result;
use crossterm::{
    cursor::Show,
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

/// Owns the terminal state: raw mode, alternate screen, mouse capture and
/// bracketed paste. Dropping it restores everything.
///
/// Raw mode clears IXON on Unix (and the Windows console input mode has no
/// XON/XOFF), so Ctrl+S / Ctrl+Q reach the app instead of freezing output.
pub struct Guard {
    _private: (),
}

impl Guard {
    pub fn enter() -> Result<Guard> {
        enable_raw_mode()?;
        // Construct the guard first so a failure below still restores via Drop.
        let guard = Guard { _private: () };
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        Ok(guard)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

/// Undo everything `Guard::enter` did. Safe to call any number of times, from
/// `Drop` and from the panic hook; "not enabled" errors are ignored.
pub fn restore() -> Result<()> {
    let mut out = io::stdout();
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        Show,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
    Ok(())
}

/// Restore the terminal, then run the previous panic hook so the message
/// prints on the main screen.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_idempotent() {
        assert!(restore().is_ok());
        assert!(restore().is_ok());
    }
}
