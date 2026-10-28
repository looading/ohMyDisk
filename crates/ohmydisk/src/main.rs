mod gui;
mod mcp;

use clap::{Parser, Subcommand};
use ohmydisk_core::AppError;
use ohmydisk_platform::NativePlatform;
use ohmydisk_service::{Client, Engine, ipc};
use serde_json::Value;
use std::{path::PathBuf, process::ExitCode, sync::Arc};

#[derive(Parser)]
#[command(
    name = "ohmydisk",
    version,
    about = "Explore disk usage and clean exact reviewed plans"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Private application state directory (defaults to the current user's local data directory)"
    )]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Open the desktop application (the default).
    Gui,
    /// Serve the local MCP protocol on standard input/output.
    Mcp {
        #[arg(long, help = "Use stdio transport (the supported default)")]
        stdio: bool,
    },
    /// Run the private local backend; normally started automatically.
    Service,
    /// Send one diagnostic JSON request to the shared local backend.
    Call {
        operation: String,
        #[arg(long, default_value = "{}")]
        args: String,
    },
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ohMyDisk: {error}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let data_dir = cli
        .data_dir
        .or_else(|| std::env::var_os("OHMYDISK_DATA_DIR").map(PathBuf::from))
        .or_else(|| dirs::data_local_dir().map(|p| p.join("ohMyDisk")))
        .ok_or_else(|| {
            AppError::new(
                "service_unavailable",
                "Cannot locate the current user's application data directory",
            )
        })?;
    let data_dir = if data_dir.is_absolute() {
        data_dir
    } else {
        std::env::current_dir()?.join(data_dir)
    };
    let command = cli.command.unwrap_or(Command::Gui);
    if matches!(command, Command::Gui) {
        gui::run(data_dir)?;
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match command {
            Command::Service => {
                // Recovery must run only after acquiring service ownership.
                let lock = ipc::acquire_service_lock(&data_dir)?;
                let engine = Engine::open(data_dir.clone(), Arc::new(NativePlatform))?;
                ipc::run_with_lock(data_dir, engine, lock).await?;
            }
            Command::Mcp { stdio: _ } => {
                mcp::run(Client::connect_or_spawn(data_dir).await?).await?
            }
            Command::Call { operation, args } => {
                let args: Value = serde_json::from_str(&args)?;
                let result = Client::connect_or_spawn(data_dir)
                    .await?
                    .call(&operation, args)
                    .await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Command::Gui => unreachable!(),
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}
