//! The Starling JavaScript runtime.
//!

// `error_chain!` can recurse deeply
#![recursion_limit = "1024"]
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
// Annoying warning emitted from the `error_chain!` macro.
#![allow(unused_doc_comment)]

#[macro_use]
extern crate derive_error_chain;
#[macro_use]
extern crate derive_is_enum_variant;
#[macro_use]
extern crate futures;
extern crate futures_cpupool;
#[macro_use]
extern crate js;
#[macro_use]
extern crate lazy_static;
extern crate num_cpus;
extern crate tokio_core;

#[macro_use]
pub mod js_native;

pub mod gc_roots;
pub(crate) mod js_global;
pub(crate) mod promise_tracker;
pub(crate) mod task;

use futures::{Sink, Stream};
use futures::sync::mpsc;
use futures_cpupool::CpuPool;
use std::cmp;
use std::collections::HashMap;
use std::fmt;
use std::mem;
use std::path;
use std::sync::Arc;
use std::thread;

/// The kind of error that occurred.
#[derive(Debug, ErrorChain)]
pub enum ErrorKind {
    /// Some other kind of miscellaneous error, described in the given string.
    Msg(String),

    /// An IO error.
    #[error_chain(foreign)]
    Io(::std::io::Error),

    /// Tried to send a value on a channel when the receiving half was already
    /// dropped.
    #[error_chain(foreign)]
    SendError(mpsc::SendError<()>),

    /// Could not create a JavaScript runtime.
    #[error_chain(custom)]
    #[error_chain(description = r#"|| "Could not create a JavaScript Runtime""#)]
    #[error_chain(display = r#"|| write!(f, "Could not create a JavaScript Runtime")"#)]
    CouldNotCreateJavaScriptRuntime,

    /// Could not read a value from a channel.
    #[error_chain(custom)]
    #[error_chain(description = r#"|| "Could not read a value from a channel""#)]
    #[error_chain(display = r#"|| write!(f, "Could not read a value from a channel")"#)]
    CouldNotReadValueFromChannel,

    /// There was an exception in JavaScript code.
    // TODO: stack, line, column, filename, etc
    #[error_chain(custom)]
    #[error_chain(description = r#"|| "JavaScript exception""#)]
    #[error_chain(display = r#"|| write!(f, "JavaScript exception")"#)]
    JavaScriptException,

    /// There was an unhandled, rejected JavaScript promise.
    // TODO: stack, line, column, filename, etc
    #[error_chain(custom)]
    #[error_chain(description = r#"|| "Unhandled, rejected JavaScript promise""#)]
    #[error_chain(display = r#"|| write!(f, "Unhandled, rejected JavaScript promise")"#)]
    JavaScriptUnhandledRejectedPromise,
}

impl Clone for Error {
    fn clone(&self) -> Self {
        self.to_string().into()
    }
}

/// Configuration options for building a Starling event loop.
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
    sync_io_pool_threads: usize,
    cpu_pool_threads: usize,
    channel_buffer_size: usize,
}

const DEFAULT_SYNC_IO_POOL_THREADS: usize = 8;
const DEFAULT_CHANNEL_BUFFER_SIZE: usize = 4096;

impl Options {
    /// Construct a new `Options` object for configuring the Starling event
    /// loop.
    ///
    /// The given `main` JavaScript file will be evaluated as the main task.
    pub fn new<P>(main: P) -> Options
    where
        P: Into<path::PathBuf>,
    {
        Options {
            main: main.into(),
            sync_io_pool_threads: DEFAULT_SYNC_IO_POOL_THREADS,
            cpu_pool_threads: num_cpus::get(),
            channel_buffer_size: DEFAULT_CHANNEL_BUFFER_SIZE,
        }
    }

    /// Configure the number of threads to reserve for the synchronous IO pool.
    ///
    /// The synchronous IO pool is a collection of threads for adapting
    /// synchronous IO libraries into the (otherwise completely asynchronous)
    /// Starling system.
    ///
    /// ### Panics
    ///
    /// Panics if `threads` is 0.
    pub fn sync_io_pool_threads(mut self, threads: usize) -> Self {
        assert!(threads > 0);
        self.sync_io_pool_threads = threads;
        self
    }

    /// Configure the number of threads to reserve for the CPU pool.
    ///
    /// The CPU pool is a collection of worker threads for CPU-bound native Rust
    /// tasks.
    ///
    /// Defaults to the number of logical CPUs on the machine.
    ///
    /// ### Panics
    ///
    /// Panics if `threads` is 0.
    pub fn cpu_pool_threads(mut self, threads: usize) -> Self {
        assert!(threads > 0);
        self.cpu_pool_threads = threads;
        self
    }

    /// Configure the size of mpsc buffers in the system.
    ///
    /// ### Panics
    ///
    /// Panics if `size` is 0.
    pub fn channel_buffer_size(mut self, size: usize) -> Self {
        assert!(size > 0);
        self.channel_buffer_size = size;
        self
    }

    /// Finish this `Options` builder and run the Starling event loop with its
    /// specified configuration.
    pub fn run(self) -> Result<()> {
        Starling::new(self)?.run()
    }
}

impl Options {
    // Get the number of `T`s that should be buffered in an mpsc channel for the
    // current configuration.
    fn buffer_capacity_for<T>(&self) -> usize {
        let size_of_t = cmp::max(1, mem::size_of::<T>());
        let capacity = self.channel_buffer_size / size_of_t;
        cmp::max(1, capacity)
    }
}

/// The Starling supervisory thread.
///
/// The supervisory thread doesn't do much other than supervise other threads: the IO
/// event loop thread, various utility thread pools, and JavaScript task
/// threads. Its primary responsibility is ensuring clean system shutdown and
/// joining thread handles.
pub(crate) struct Starling {
    handle: StarlingHandle,
    receiver: mpsc::Receiver<StarlingMessage>,

    // Currently there is a 1:1 mapping between JS tasks and native
    // threads. That is expected to change in the future, hence the
    // distinction between `self.tasks` and `self.threads`.
    tasks: HashMap<task::TaskId, task::TaskHandle>,
    threads: HashMap<thread::ThreadId, thread::JoinHandle<()>>,
}

impl fmt::Debug for Starling {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Starling {{ .. }}")
    }
}

impl Starling {
    /// Construct a Starling system from the given options.
    pub fn new(opts: Options) -> Result<Starling> {
        let tasks = HashMap::new();
        let threads = HashMap::new();

        let (sender, receiver) = mpsc::channel(opts.buffer_capacity_for::<StarlingMessage>());

        let Options {
            sync_io_pool_threads,
            cpu_pool_threads,
            ..
        } = opts;

        let handle = StarlingHandle {
            options: Arc::new(opts),
            sync_io_pool: CpuPool::new(sync_io_pool_threads),
            cpu_pool: CpuPool::new(cpu_pool_threads),
            sender,
        };

        Ok(Starling {
            handle,
            receiver,
            tasks,
            threads,
        })
    }

    /// Run the main Starling event loop with the specified options.
    pub fn run(mut self) -> Result<()> {
        let (main, thread) =
            task::Task::spawn_main(self.handle.clone(), self.handle.options().main.clone())?;

        self.tasks.insert(main.id(), main.clone());
        self.threads.insert(thread.thread().id(), thread);

        for msg in self.receiver.wait() {
            let msg = msg.map_err(|_| {
                Error::from_kind(ErrorKind::CouldNotReadValueFromChannel)
            })?;

            match msg {
                StarlingMessage::TaskFinished(id) => {
                    assert!(self.tasks.remove(&id).is_some());

                    let thread_id = id.into();
                    let join_handle = self.threads
                        .remove(&thread_id)
                        .expect("should have a thread join handle for the finished task");
                    join_handle
                        .join()
                        .expect("should join finished task's thread OK");

                    if id == main.id() {
                        // TODO: notification of shutdown and joining other threads and things.
                        return Ok(());
                    }
                }

                StarlingMessage::TaskErrored(id, error) => {
                    assert!(self.tasks.remove(&id).is_some());

                    let thread_id = id.into();
                    let join_handle = self.threads
                        .remove(&thread_id)
                        .expect("should have a thread join handle for the errored task");
                    join_handle
                        .join()
                        .expect("should join errored task's thread OK");

                    if id == main.id() {
                        // TODO: notification of shutdown and joining other threads and things.
                        return Err(error);
                    }
                }

                StarlingMessage::NewTask(_task_handle, _join_handle) => unimplemented!(),
            }
        }

        Ok(())
    }
}

/// Messages that threads can send to the Starling supervisory thread.
#[derive(Debug)]
pub(crate) enum StarlingMessage {
    /// The task on the given thread completed successfully.
    TaskFinished(task::TaskId),

    /// The task on the given thread failed with the given error.
    TaskErrored(task::TaskId, Error),

    /// A new child task was created.
    NewTask(task::TaskId, thread::JoinHandle<()>),
}

/// A handle to the Starling system.
///
/// A `StarlingHandle` is a capability to schedule IO on the event loop, spawn
/// work in one of the utility thread pools, and communicate with the Starling
/// supervisory thread. Handles can be cloned and sent across threads,
/// propagating these capabilities.
#[derive(Clone)]
pub(crate) struct StarlingHandle {
    options: Arc<Options>,
    sync_io_pool: CpuPool,
    cpu_pool: CpuPool,
    sender: mpsc::Sender<StarlingMessage>,
}

impl fmt::Debug for StarlingHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "StarlingHandle {{ .. }}")
    }
}

impl StarlingHandle {
    /// Get the `Options` that this Starling system was configured with.
    pub fn options(&self) -> &Arc<Options> {
        &self.options
    }

    /// Get a handle to the thread pool for adapting synchronous IO (perhaps
    /// from a library that wasn't written to be async) into the system.
    pub fn sync_io_pool(&self) -> &CpuPool {
        &self.sync_io_pool
    }

    /// Get a handle to the thread pool for performing CPU-bound native Rust
    /// tasks.
    pub fn cpu_pool(&self) -> &CpuPool {
        &self.cpu_pool
    }

    /// Send a message to the Starling supervisory thread.
    pub fn send(&self, msg: StarlingMessage) -> futures::sink::Send<mpsc::Sender<StarlingMessage>> {
        self.sender.clone().send(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task::{TaskHandle, TaskMessage};

    fn assert_clone<T: Clone>() {}
    fn assert_send<T: Send>() {}

    #[test]
    fn error_is_send() {
        assert_send::<Error>();
    }

    #[test]
    fn options_is_send_clone() {
        assert_clone::<Options>();
        assert_send::<Options>();
    }

    #[test]
    fn starling_handle_is_send_clone() {
        assert_clone::<StarlingHandle>();
        assert_send::<StarlingHandle>();
    }

    #[test]
    fn task_handle_is_send_clone() {
        assert_clone::<TaskHandle>();
        assert_send::<TaskHandle>();
    }

    #[test]
    fn starling_message_is_send() {
        assert_send::<StarlingMessage>();
    }

    #[test]
    fn task_message_is_send() {
        assert_send::<TaskMessage>();
    }
}
