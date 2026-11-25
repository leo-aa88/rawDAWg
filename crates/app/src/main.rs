//! rawDAWg executable: CLI entry (headless render, device listing, safe mode)
//! and the native dark GUI.

mod audio;
mod cli;
mod theme;
mod ui;

use clap::Parser;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp_millis().init();
    let args = cli::Args::parse();
    let code = match cli::run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rawDAWg: {e:#}");
            2
        }
    };
    std::process::exit(code);
}
