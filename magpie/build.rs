use std::{env, path::PathBuf};

fn main() {
    let _out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
}
