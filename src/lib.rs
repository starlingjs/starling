//! The project that will henceforth be known as `starling`.
//!
// `error_chain!` can recurse deeply
#![recursion_limit = "1024"]
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(unsafe_code)]
// Annoying warning emitted from the `error_chain!` macro.
#![allow(unused_doc_comment)]

#[macro_use]
extern crate error_chain;
extern crate futures;
extern crate tokio_core;

use futures::future;
use std::path;
use tokio_core::reactor::Core;

error_chain! {
    foreign_links {
        Io(::std::io::Error)
        /// An IO error.
            ;
    }
}

/// Configuration options for building a `starling` event loop.
///
/// ```
/// extern crate starling;
///
/// # fn foo() -> starling::Result<()> {
/// // Construct a new `Options` builder, providing the file containing
/// // the main JavaScript task.
/// starling::Options::new("path/to/main.js")
///     // Finish configuring the `Options` builder and run the event
///     // loop!
///     .run()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Options {
    main: path::PathBuf,
}

impl Options {
    /// Construct a new `Options` object for configuring the `starling` event
    /// loop.
    ///
    /// The given `main` JavaScript file will be evaluated as the main task.
    pub fn new<P>(main: P) -> Options
    where
        P: Into<path::PathBuf>,
    {
        Options { main: main.into() }
    }

    /// Finish this `Options` builder and run the `starling` event loop with its
    /// specified configuration.
    pub fn run(self) -> Result<()> {
        run_with_options(self)
    }
}

/// Run the main `starling` event loop with the specified options.
fn run_with_options(_opts: Options) -> Result<()> {
    let mut core = Core::new()?;
    core.run(future::ok(()))
}

#[test]
fn it_works() {}
