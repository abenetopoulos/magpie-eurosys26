// Build script that sets an environment variable to the current cargo profile, which we need
// when we are parsing dependency information to emit correct resolvers for epics.
use std::{env, path::PathBuf};

const VAR_NAME: &'static str = "DEP_PROFILE";

fn main() {
    if std::env::var(VAR_NAME).is_ok() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap())
        .into_os_string()
        .into_string()
        .unwrap();

    let target = std::env::var("TARGET").expect("could not get target");
    let host_triple = std::env::var("HOST").expect("could not get host");

    let profile = match std::env::var("PROFILE") {
        Ok(p) => match target == host_triple {
            false => format!("{}/{}", target, p),
            true => p,
        },
        Err(_) => match out_dir.contains("release") {
            false => "debug",
            true => "release",
        }
        .to_string(),
    };
    println!("cargo:rustc-env={}={}", VAR_NAME, profile);
}
