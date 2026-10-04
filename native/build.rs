use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").expect("Cargo supplies RUSTC");
    let output = Command::new(rustc)
        .arg("--version")
        .output()
        .expect("query build compiler");
    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).expect("rustc version is UTF-8");
    println!("cargo:rustc-env=LEDFX_RUSTC_VERSION={}", version.trim());
    println!(
        "cargo:rustc-env=LEDFX_BUILD_PROFILE={}",
        std::env::var("PROFILE").expect("Cargo supplies PROFILE")
    );
    println!("cargo:rerun-if-changed=build.rs");
}
