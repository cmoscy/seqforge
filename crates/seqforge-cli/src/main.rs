use clap::Parser;
use seqforge_cli::Commands;

#[derive(Parser)]
#[command(name = "seqforge", about = "SeqForge sequence tool CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Info { ref path } => seqforge_cli::run_info(path),
    }
}
