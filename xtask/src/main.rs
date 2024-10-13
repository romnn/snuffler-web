mod embed_python;

use color_eyre::eyre;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
pub enum Command {
    PrepareEmbedPython,
}

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    // /// Name of the person to greet
    // #[arg(short, long)]
    // name: String,
    //
    // /// Number of times to greet
    // #[arg(short, long, default_value_t = 1)]
    // count: u8,

    #[command(subcommand)]
    command: Command
}

fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    env_logger::init();

    let args = Args::parse();
    let dest = PathBuf::from("./embed-dest");
    match args.command {
        Command::PrepareEmbedPython => {
            embed_python::generate_python_embedding_artifacts(&dest)?;
        },
    }
    Ok(())
}
