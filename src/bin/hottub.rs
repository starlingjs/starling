extern crate hottub;

use std::process;

fn main() {
    if let Err(e) = hottub::run() {
        println!("Error: {}", e);
        process::exit(1);
    }
}
