use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {
    #[arg(short('c'), long)]
    pub config_file_path: Option<PathBuf>,

    #[arg(short('r'), long, default_value("false"))]
    pub reset_object_dir: bool,

    #[arg(short('f'), long)]
    pub function: Option<String>,
    #[arg(short('s'), long)]
    pub seed_data: Option<PathBuf>,
    #[arg(short('w'), long)]
    pub driver_workload: Option<PathBuf>,
    #[arg(short('n'), long, default_value("1"))]
    pub num_rounds: u16,
}
