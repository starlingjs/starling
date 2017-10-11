extern crate starling;

use std::path::Path;
use std::process::Command;

fn assert_starling_run_file<P>(
    path: P,
    expect_success: bool,
    stdout_has: &[&'static str]
)
where
    P: AsRef<Path>,
{
    let path = path.as_ref().display().to_string();

    let output = Command::new(env!("STARLING_TEST_EXECUTABLE"))
        .arg(&path)
        .output()
        .expect("Should spawn `cargo run` OK");

    let was_success = output.status.success();

    assert_eq!(
        expect_success,
        was_success,
        "should have expected exit status for {}",
        path
    );

    for s in stdout_has {
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(s),
            "should have '{}' in stdout for {}",
            s,
            path
        );
    }
}

include!(concat!(env!("OUT_DIR"), "/js_tests.rs"));
