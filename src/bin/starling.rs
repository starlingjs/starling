extern crate starling;

use std::process;

fn main() {
    if let Err(e) = starling::run() {
        println!("Error: {}", e);
        process::exit(1);
    }
}
