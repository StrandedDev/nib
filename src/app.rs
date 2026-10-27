use std::io::{self, Write};

use anyhow::Result;
use crossterm::{
    cursor::MoveTo,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    queue,
    terminal::{Clear, ClearType},
};

use crate::terminal::Guard;

pub struct App;

impl App {
    pub fn new() -> Result<App> {
        Ok(App)
    }

    pub fn run(&mut self) -> Result<()> {
        // Dropped on return, so the terminal is restored before main prints errors.
        let _guard = Guard::enter()?;
        draw()?;
        loop {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    // Hard-coded until the command registry arrives.
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('q')
                    {
                        return Ok(());
                    }
                }
                Event::Resize(..) => draw()?,
                _ => {}
            }
        }
    }
}

fn draw() -> Result<()> {
    let mut out = io::stdout();
    queue!(out, Clear(ClearType::All), MoveTo(0, 0))?;
    out.flush()?;
    Ok(())
}
