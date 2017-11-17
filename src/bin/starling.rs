extern crate clap;
extern crate starling;

use clap::{App, Arg};
use std::process;

/// Parse the given CLI arguments into a `starling::Options` configuration
/// object.
///
/// If argument parsing fails, then a usage string is printed, and the process
/// is exited with 1.
fn parse_cli_args() -> starling::Options {
    let matches = App::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .arg(
            Arg::with_name("file")
                .required(true)
                .help("The JavaScript file to evaluate as the main task."),
        )
        .get_matches();

    let opts = starling::Options::new(matches.value_of("file").unwrap());

    opts
}

fn main() {
    let opts = parse_cli_args();
    if let Err(e) = opts.run() {
        eprintln!("error: {}", e);
        process::exit(1);
    }
}
