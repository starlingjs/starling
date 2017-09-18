extern crate starling;

use std::path::Path;
use std::process::Command;

fn assert_starling_run_file<P: AsRef<Path>>(path: P, expect_success: bool) {
    let path = path.as_ref().display().to_string();
    let was_success = Command::new(env!("STARLING_TEST_EXECUTABLE"))
        .arg(&path)
        .status()
        .expect("Should spawn `cargo run` OK")
        .success();

    assert_eq!(
        expect_success,
        was_success,
        "should have expected exit status for {}",
        path
    );
}

include!(concat!(env!("OUT_DIR"), "/js_tests.rs"));
