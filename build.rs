use std::{env, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=FAYLS_COMMIT");

    // Follow the current ref so a new commit updates the embedded hash.
    for path in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &reference]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let commit = env::var("FAYLS_COMMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| git(&["rev-parse", "--short=8", "HEAD"]))
        .expect("Git commit unavailable; set FAYLS_COMMIT when building without Git metadata");

    println!("cargo:rustc-env=FAYLS_COMMIT={commit}");
    println!(
        "cargo:rustc-env=FAYLS_PROFILE={}",
        env::var("PROFILE").expect("Cargo should set PROFILE")
    );
}
