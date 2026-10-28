use std::path::PathBuf;
use std::process::ExitCode;

use nib::{
    app::App,
    cli::{self, Args},
    clipboard, config,
    recovery::Recovery,
    terminal,
};

fn main() -> ExitCode {
    let (path, line) = match cli::parse(std::env::args_os().skip(1)) {
        Ok(Args::Version) => {
            println!("{}", cli::version_text());
            return ExitCode::SUCCESS;
        }
        Ok(Args::Help) => {
            println!("{}", cli::help_text());
            return ExitCode::SUCCESS;
        }
        Ok(Args::Edit { path, .. }) => target(path),
        Err(msg) => {
            eprintln!("nib: {msg}");
            return ExitCode::from(2);
        }
    };
    terminal::install_panic_hook();
    // The guard lives inside `run`, so the terminal is restored before we print.
    let (config, warning) = config::load();
    let result = App::open_configured(path, line, Recovery::system(), config).and_then(|app| {
        let mut app = app.with_clipboard(clipboard::detect());
        if let Some(warning) = warning {
            app.set_status(&warning);
        }
        app.run()?;
        Ok(app.exit_note().map(str::to_string))
    });
    match result {
        Ok(note) => {
            if let Some(note) = note {
                eprintln!("nib: {note}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nib: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// The file to edit and the line to start on. A non-UTF-8 argument is used
/// as given: no `:line` suffix and no `.txt` rule.
fn target(raw: Option<PathBuf>) -> (Option<PathBuf>, Option<usize>) {
    let Some(raw) = raw else {
        return (None, None);
    };
    let (path, line) = match raw.to_str() {
        Some(s) => cli::resolve(s, |p| p.exists()),
        None => (raw, None),
    };
    let path = std::path::absolute(&path).unwrap_or(path);
    (Some(path), line)
}
