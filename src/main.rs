#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("mextdisplay supports Apple Silicon Macs only");

mod app;
mod display;
mod state;

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::display::{Display, DisplayManager, DisplayStatus};

#[derive(Debug, Parser)]
#[command(
    name = "mextdisplay",
    version,
    about = "Manage external displays on Apple Silicon Macs"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List displays and their stable identifiers
    List,
    /// Disable an external display for this login session
    #[command(alias = "off")]
    Disable {
        /// Display UUID, UUID prefix, numeric ID, or name
        display: String,
    },
    /// Re-enable a display disabled by mextdisplay
    #[command(alias = "on")]
    Enable {
        /// Display UUID, UUID prefix, numeric ID, or name
        display: String,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let manager = DisplayManager::new()?;

    match cli.command {
        None => app::run(manager),
        Some(Command::List) => print_displays(manager.refresh()?),
        Some(Command::Disable { display }) => {
            let displays = manager.refresh()?;
            let selected = display::resolve_display(&displays, &display)?;
            println!("{}", manager.disable(&selected.uuid)?);
            Ok(())
        }
        Some(Command::Enable { display }) => {
            let displays = manager.refresh()?;
            let selected = display::resolve_display(&displays, &display)?;
            println!("{}", manager.enable(&selected.uuid)?);
            Ok(())
        }
    }
}

fn print_displays(displays: Vec<Display>) -> Result<()> {
    if displays.is_empty() {
        println!("No displays found.");
        return Ok(());
    }

    println!("{:<24} {:<10} {:<8} UUID", "NAME", "STATUS", "ID");
    for display in displays {
        let status = match display.status {
            DisplayStatus::Enabled => "enabled",
            DisplayStatus::Disabled => "disabled",
        };
        let mut role = Vec::new();
        if display.builtin {
            role.push("built-in");
        }
        if display.main {
            role.push("main");
        }
        let role = if role.is_empty() {
            String::new()
        } else {
            format!(" ({})", role.join(", "))
        };
        println!(
            "{:<24} {:<10} {:<8} {}{}",
            truncate(&display.name, 24),
            status,
            display.id,
            display.uuid,
            role
        );
    }
    Ok(())
}

fn truncate(value: &str, width: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(width.saturating_sub(1)).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        value.to_owned()
    }
}
