use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    #[arg(short('c'), long)]
    pub config_file_path: Option<PathBuf>,

    #[arg(short('s'), long)]
    pub seed_data: Option<PathBuf>,
}
