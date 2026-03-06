use std::env;
use std::fs::read_to_string;
use std::path::PathBuf;

use serde_derive::{Deserialize, Serialize};
use toml;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub hosts: Vec<String>,
    pub port: u16,
}

impl Config {
    pub fn init_from_file(config_file: PathBuf) -> Self {
        let config_str = read_to_string(&config_file)
            .expect(&format!("failed to open config file {:#?}", config_file));

        let config: Self = toml::from_str(&config_str).unwrap();
        config
    }
}
