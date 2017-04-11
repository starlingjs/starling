//! The project that will henceforth be known as `hottub`.

#![deny(missing_docs)]
#![deny(missing_debug_implementations)]

extern crate futures;
extern crate tokio_core;

use futures::future;
use std::io;
use tokio_core::reactor::Core;

/// Run the main `hottub` loop.
pub fn run() -> Result<(), io::Error> {
    let mut core = Core::new()?;
    core.run(future::ok(()))
}

#[test]
fn it_works() {
}
