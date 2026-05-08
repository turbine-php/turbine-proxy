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

    // Allow CI to pre-build the dashboard (e.g. before `cross build` which runs
    // inside a Docker container without Node.js) and skip the npm step here.
    // Set TURBINEPROXY_SKIP_DASHBOARD_BUILD=1 in the environment to skip.
    let skip = std::env::var("TURBINEPROXY_SKIP_DASHBOARD_BUILD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // Also skip when dashboard/dist already contains built assets — this covers
    // the case where the CI job pre-builds the frontend in a prior step.
    let dist_exists = dashboard_dir.join("dist").join("index.html").exists();

    if skip || dist_exists {
        return;
    }

    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(&dashboard_dir)
        .status()
        .expect("failed to run `npm run build` in dashboard/");

    if !status.success() {
        panic!("dashboard npm build failed");
    }
}
