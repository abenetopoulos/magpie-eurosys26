use std::{env, path::PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap())
        .into_os_string()
        .into_string()
        .unwrap();
    // NOTE this is kinda wrong because we're ignoring any other kind of profile in cases where
    // we're building without "PROFILE" being set, but will do for now.
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
    println!("cargo:rustc-env=DEP_PROFILE={}", profile);
}
