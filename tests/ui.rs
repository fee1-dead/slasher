use std::process::Command;

use ui_test::color_eyre::eyre::bail;
use ui_test::{run_tests, Config};

fn main() -> ui_test::color_eyre::Result<()> {
    if !Command::new("cargo").args(["build", "--release"]).status()?.success() {
        bail!("cargo process failed")
    }
    unsafe {
        std::env::set_var("RUSTC", "./target/release/slasher");
    }
    let config = Config::rustc("tests/ui");
    let abort_check = config.abort_check.clone();
    ctrlc::set_handler(move || abort_check.abort())?;

    // Compile all `.rs` files in the given directory (relative to your
    // Cargo.toml) and compare their output against the corresponding
    // `.stderr` files.
    run_tests(config)
}
