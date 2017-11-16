use futures::sync::mpsc;
use std::io;

/// The kind of error that occurred.
#[derive(Debug, ErrorChain)]
pub enum ErrorKind {
    /// Some other kind of miscellaneous error, described in the given string.
    Msg(String),

    /// An IO error.
    #[error_chain(foreign)]
    Io(io::Error),

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

    /// The JavaScript `Promise` that was going to settle this future was
    /// reclaimed by the garbage collector without having been resolved or
    /// rejected.
    #[error_chain(custom)]
    #[error_chain(description = r#"|| "JavaScript Promise collected without settling""#)]
    #[error_chain(display = r#"|| write!(f, "JavaScript Promise collected without settling")"#)]
    JavaScriptPromiseCollectedWithoutSettling,
}

impl Clone for Error {
    fn clone(&self) -> Self {
        self.to_string().into()
    }
}
