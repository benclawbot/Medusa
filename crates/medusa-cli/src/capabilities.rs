use std::path::PathBuf;

use clap::Parser;
use medusa_capabilities::CapabilityRegistry;

/// Print the single validated capability snapshot used by the product.
#[derive(Debug, Parser)]
#[command(name = "medusa-capabilities")]
struct Cli {
    /// Repository path used for capability discovery.
    #[arg(value_name = "REPOSITORY_PATH", default_value = ".")]
    repository_path: PathBuf,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let registry = CapabilityRegistry::discover(&cli.repository_path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&registry.protocol_report())?
    );
    Ok(())
}
