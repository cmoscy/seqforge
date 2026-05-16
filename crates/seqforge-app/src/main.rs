mod app;
mod browser;
mod cli_install;
mod command;
mod event;
mod focus;
mod keymap;
mod overlay;
mod socket;
mod tabs;
mod terminal;
mod viewer;
mod workspace;

use clap::Parser;

#[derive(Parser)]
#[command(name = "seqforge-app", about = "SeqForge sequence viewer")]
struct Args {
    /// Install the bundled `seqforge` CLI to PATH and exit.
    /// Symlinks the CLI binary into /usr/local/bin or ~/.local/bin.
    #[arg(long)]
    install_cli: bool,
}

fn main() -> eframe::Result {
    let args = Args::parse();

    if args.install_cli {
        match cli_install::install_cli_to_path() {
            Ok(r) => {
                println!(
                    "seqforge installed to {}{}",
                    r.target.display(),
                    if r.was_updated { " (updated)" } else { "" }
                );
                println!(
                    "Make sure {} is on your PATH.",
                    r.target.parent().unwrap().display()
                );
            }
            Err(e) => {
                eprintln!("Install failed: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    let options = eframe::NativeOptions {
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native(
        "SeqForge",
        options,
        Box::new(|cc| Ok(Box::new(app::SeqForgeApp::new(cc)))),
    )
}
