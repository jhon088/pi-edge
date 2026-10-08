//! Entry point of the PI.Edge runtime.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use edge_daemon::{run, DaemonConfig};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "edge-daemon",
    version,
    about = "PI.Edge headless POS runtime. Owns the local database and serves local clients."
)]
struct Args {
    /// Database owned by this runtime. The IPC endpoint, runtime lock and
    /// local credential are derived from it.
    #[arg(long, env = "PI_EDGE_DB", default_value = "./data/pi-edge.db")]
    db: PathBuf,

    /// Suppress the machine-readable readiness line on stdout.
    #[arg(long)]
    quiet: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    init_tracing();

    let mut config = DaemonConfig::new(args.db);
    config.announce_ready = !args.quiet;

    match run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if error.is_single_writer_violation() {
                // Expected condition, not a crash: say it plainly and leave the
                // database untouched.
                tracing::error!(%error, "refusing to start a second runtime");
            } else {
                tracing::error!(%error, "edge daemon failed to start");
            }
            eprintln!("edge-daemon: {error}");
            ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1))
        }
    }
}

/// Logs go to stderr; stdout is reserved for the readiness line.
fn init_tracing() {
    let filter = EnvFilter::try_from_env("PI_EDGE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
