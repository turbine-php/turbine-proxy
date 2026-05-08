use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dashboard_dir = manifest_dir.join("dashboard");

    // Re-run if any dashboard source file changes
    println!("cargo:rerun-if-changed=dashboard/src");
    println!("cargo:rerun-if-changed=dashboard/index.html");
    println!("cargo:rerun-if-changed=dashboard/package.json");
    println!("cargo:rerun-if-changed=dashboard/vite.config.js");

    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(&dashboard_dir)
        .status()
        .expect("failed to run `npm run build` in dashboard/");

    if !status.success() {
        panic!("dashboard npm build failed");
    }
}
