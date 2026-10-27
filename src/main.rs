use std::process::ExitCode;

use nib::{app::App, terminal};

fn main() -> ExitCode {
    terminal::install_panic_hook();
    // The guard lives inside `run`, so the terminal is restored before we print.
    let result = App::new().and_then(|mut app| app.run());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("nib: {err:#}");
            ExitCode::FAILURE
        }
    }
}
