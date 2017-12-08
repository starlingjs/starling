//! Gluing Rust's `Future` and JavaScript's `Promise` together.
//!
//! JavaScript's `Promise` and Rust's `Future` are two abstractions with the
//! same goal: asynchronous programming with eventual values. Both `Promise`s
//! and `Future`s represent a value that may or may not have been computed
//! yet. However, the way that you interact with each of them is very different.
//!
//! JavaScript's `Promise` follows a completion-based model. You register
//! callbacks on a promise, and the runtime invokes each of the registered
//! callbacks (there may be many!) when the promise is resolved with a
//! value. You build larger asynchronous computations by chaining promises;
//! registering a callback with a promise returns a new promise of that
//! callback's result. Dependencies between promises are managed by the runtime.
//!
//! Rust's `Future` follows a readiness-based model. You define a `poll` method
//! that either returns the future's value if it is ready, or a sentinel that
//! says the future's value is not ready yet. You build larger asynchronous
//! computations by defining (or using existing) combinators that wrap some
//! inner future (or futures). These combinators delegate polling to the inner
//! future, and once the inner future's value is ready, perform their
//! computation on the value, and return the transformed result. Dependencies
//! between futures are managed by the futures themselves, while scheduling
//! polling is the runtime's responsibility.
//!
//! To translate between futures and promises we take two different approaches
//! for each direction, by necessity.
//!
//! To treat a Rust `Future` as a JavaScript `Promise`, we define a `Future`
//! combinator called `Future2Promise`. It takes a fresh, un-resolved `Promise`
//! object, and an inner future upon construction. It's `poll` method delegates
//! to the inner future's `poll`, and once the inner future's value is ready, it
//! resolves the promise with the ready value, and the JavaScript runtime
//! ensures that the promise's registered callbacks are invoked appropriately.
//!
//! To treat a JavaScript `Promise` as a Rust `Future`, we register a callback
//! to the promise that sends the resolution value over a one-shot
//! channel. One-shot channels are split into their two halves: sender and
//! receiver. The sender half moves into the callback, but the receiver half is
//! a future, and it represents the future resolution value of the original
//! promise.
//!
//! The final concern to address is that the runtime scheduling the polling of
//! Rust `Future`s (`tokio`) still knows which futures to poll despite it only
//! seeing half the picture now. I say "half the picture" because dependencies
//! that would otherwise live fully within the futures ecosystem are now hidden
//! in promises inside the JavaScript runtime.
//!
//! First, we must understand how `tokio` schedules polling. It is not busy
//! spinning and calling `poll` continuously in a loop. `tokio` maintains a set
//! of "root" futures. These are the futures passed to `Core::run` and
//! `Handle::spawn` directly. When `tokio` polls a "root" future, that `poll`
//! call will transitively reach down and call `poll` on "leaf" futures that
//! wrap file descriptors and sockets and such things. It is these "leaf"
//! futures' responsibilty to use OS APIs to trigger wake ups when new data is
//! available on a socket or what have you, and then it is `tokio`'s
//! responsibilty to map that wake up back to which "root" future it should poll
//! again. If the "leaf" futures do not properly register to be woken up again,
//! `tokio` will never poll that "root" future again, effectively dead locking
//! it.
//!
//! So we must ensure that our `Promise`-backed futures will always be polled
//! again by making sure that they have proper "leaf" futures. Luckily, the
//! receiver half of a one-shot channel is such a "leaf" future that properly
//! registers future wake ups. If instead, for example, we tried directly
//! checking the promise's state in `poll` with JSAPI methods, we *wouldn't*
//! register any wake ups, `tokio` would never `poll` the future again, and the
//! future would dead lock.

use super::{Error, ErrorKind};
use futures::{self, Async, Future, Poll, Select};
use futures::sync::oneshot;
use future_ext::{ready, FutureExt};
use gc_roots::GcRoot;
use js::conversions::{ConversionResult, FromJSValConvertible, ToJSValConvertible};
use js::glue::ReportError;
use js::jsapi;
use js::jsval;
use js::rust::Runtime as JsRuntime;
use state_machine_future::RentToOwn;
use std::marker::PhantomData;
use std::os::raw;
use std::ptr;
use task;
use void::Void;

type GenericVoid<T> = (Void, PhantomData<T>);

/// A future that resolves a promise with its inner future's value when ready.
#[derive(StateMachineFuture)]
#[allow(dead_code)]
enum Future2Promise<F>
where
    F: Future,
    <F as Future>::Item: ToJSValConvertible,
    <F as Future>::Error: ToJSValConvertible,
{
    /// Initially, we are waiting on the inner future to be ready or error.
    #[state_machine_future(start, transitions(Finished, NotifyingOfError))]
    WaitingOnInner {
        future: F,
        promise: GcRoot<*mut jsapi::JSObject>,
    },

    /// If we encountered an error that needs to propagate, we must send it to
    /// the task.
    #[state_machine_future(transitions(Finished))]
    NotifyingOfError {
        notify: futures::sink::Send<futures::sync::mpsc::Sender<task::TaskMessage>>,
        phantom: PhantomData<F>,
    },

    /// All done.
    #[state_machine_future(ready)]
    Finished(PhantomData<F>),

    /// We explicitly handle all errors, so make `Future::Error` impossible to
    /// construct.
    #[state_machine_future(error)]
    Impossible(GenericVoid<F>),
}

impl<F> PollFuture2Promise<F> for Future2Promise<F>
where
    F: Future,
    <F as Future>::Item: ToJSValConvertible,
    <F as Future>::Error: ToJSValConvertible,
{
    fn poll_waiting_on_inner<'a>(
        waiting: &'a mut RentToOwn<'a, WaitingOnInner<F>>,
    ) -> Poll<AfterWaitingOnInner<F>, GenericVoid<F>> {
        let error = match waiting.future.poll() {
            Ok(Async::NotReady) => {
                return Ok(Async::NotReady);
            }
            Ok(Async::Ready(t)) => {
                let cx = JsRuntime::get();

                unsafe {
                    rooted!(in(cx) let mut val = jsval::UndefinedValue());
                    t.to_jsval(cx, val.handle_mut());

                    rooted!(in(cx) let promise = waiting.promise.raw());
                    assert!(jsapi::JS::ResolvePromise(
                        cx,
                        promise.handle(),
                        val.handle()
                    ));

                    if let Err(e) = task::drain_micro_task_queue() {
                        e
                    } else {
                        return ready(Finished(PhantomData));
                    }
                }
            }
            Err(error) => {
                let cx = JsRuntime::get();

                unsafe {
                    rooted!(in(cx) let mut val = jsval::UndefinedValue());
                    error.to_jsval(cx, val.handle_mut());

                    rooted!(in(cx) let promise = waiting.promise.raw());
                    assert!(jsapi::JS::RejectPromise(cx, promise.handle(), val.handle()));

                    if let Err(e) = task::drain_micro_task_queue() {
                        e
                    } else {
                        return ready(Finished(PhantomData));
                    }
                }
            }
        };

        let msg = task::TaskMessage::UnhandledRejectedPromise { error };
        ready(NotifyingOfError {
            notify: task::this_task().send(msg),
            phantom: PhantomData,
        })
    }

    fn poll_notifying_of_error<'a>(
        notification: &'a mut RentToOwn<'a, NotifyingOfError<F>>,
    ) -> Poll<AfterNotifyingOfError<F>, GenericVoid<F>> {
        match notification.notify.poll() {
            Ok(Async::NotReady) => {
                return Ok(Async::NotReady);
            }
            // The only way we can get an error here is if we lost a
            // race between notifying the task of an error and the task
            // finishing.
            Err(_) | Ok(Async::Ready(_)) => ready(Finished(PhantomData)),
        }
    }
}

/// Convert a Rust `Future` into a JavaScript `Promise`.
///
/// The `Future` is spawned onto the current thread's `tokio` event loop.
pub fn future_to_promise<F, T, E>(future: F) -> GcRoot<*mut jsapi::JSObject>
where
    F: 'static + Future<Item = T, Error = E>,
    T: 'static + ToJSValConvertible,
    E: 'static + ToJSValConvertible,
{
    let cx = JsRuntime::get();
    rooted!(in(cx) let executor = ptr::null_mut());
    rooted!(in(cx) let proto = ptr::null_mut());
    rooted!(in(cx) let promise = unsafe {
        jsapi::JS::NewPromiseObject(
            cx,
            executor.handle(),
            proto.handle()
        )
    });
    assert!(!promise.get().is_null());
    let promise = GcRoot::new(promise.get());
    let future = Future2Promise::start(future, promise.clone()).ignore_results();
    task::event_loop().spawn(future);
    promise
}

const CLOSURE_SLOT: usize = 0;

// JSNative that forwards the call `f`.
unsafe extern "C" fn trampoline<F>(
    cx: *mut jsapi::JSContext,
    argc: raw::c_uint,
    vp: *mut jsapi::JS::Value,
) -> bool
where
    F: 'static + FnOnce(*mut jsapi::JSContext, &jsapi::JS::CallArgs) -> bool,
{
    let args = jsapi::JS::CallArgs::from_vp(vp, argc);
    rooted!(in(cx) let callee = args.callee());

    let private = jsapi::js::GetFunctionNativeReserved(callee.get(), CLOSURE_SLOT);
    let f = (*private).to_private() as *mut F;
    if f.is_null() {
        ReportError(cx, b"May only be called once\0".as_ptr() as *const _);
        return false;
    }

    let private = jsval::PrivateValue(ptr::null());
    jsapi::js::SetFunctionNativeReserved(callee.get(), CLOSURE_SLOT, &private);

    let f = Box::from_raw(f);
    f(cx, &args)
}

/// This is unsafe because the resulting function object will _not_ trace `f`'s
/// closed over values. Don't close over GC things!
unsafe fn make_js_fn<F>(f: F) -> GcRoot<*mut jsapi::JSObject>
where
    F: 'static + FnOnce(*mut jsapi::JSContext, &jsapi::JS::CallArgs) -> bool,
{
    let cx = JsRuntime::get();

    rooted!(in(cx) let func = jsapi::js::NewFunctionWithReserved(
        cx,
        Some(trampoline::<F>),
        0,              // nargs
        0,              // flags
        ptr::null_mut() // name
    ));
    assert!(!func.get().is_null());

    let private = Box::new(f);
    let private = jsval::PrivateValue(Box::into_raw(private) as *const _);
    jsapi::js::SetFunctionNativeReserved(func.get() as *mut _, CLOSURE_SLOT, &private);

    GcRoot::new(func.get() as *mut jsapi::JSObject)
}

type ResultReceiver<T, E> = oneshot::Receiver<super::Result<Result<T, E>>>;

/// A future of either a JavaScript promise's resolution `T` or rejection `E`.
pub struct Promise2Future<T, E>
where
    T: 'static + FromJSValConvertible<Config = ()>,
    E: 'static + FromJSValConvertible<Config = ()>,
{
    inner: Select<ResultReceiver<T, E>, ResultReceiver<T, E>>,
}

impl<T, E> ::std::fmt::Debug for Promise2Future<T, E>
where
    T: 'static + FromJSValConvertible<Config = ()>,
    E: 'static + FromJSValConvertible<Config = ()>,
{
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "Promise2Future {{ .. }}")
    }
}

impl<T, E> Future for Promise2Future<T, E>
where
    T: 'static + FromJSValConvertible<Config = ()>,
    E: 'static + FromJSValConvertible<Config = ()>,
{
    type Item = Result<T, E>;
    type Error = super::Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        match self.inner.poll() {
            Err((oneshot::Canceled, _)) => {
                Err(ErrorKind::JavaScriptPromiseCollectedWithoutSettling.into())
            }
            Ok(Async::NotReady) => Ok(Async::NotReady),
            // One of the handlers was called, but then we encountered an error
            // converting the value from JS into Rust or something like that.
            Ok(Async::Ready((Err(e), _))) => Err(e),
            Ok(Async::Ready((Ok(result), _))) => Ok(Async::Ready(result)),
        }
    }
}

/// Convert the given JavaScript `Promise` object into a future.
///
/// The resulting future is of either an `Ok(T)` if the promise gets resolved,
/// or an `Err(E)` if the promise is rejected.
///
/// Failure to convert the resolution or rejection JavaScript value into a `T`
/// or `E` will cause the resulting future's `poll` to return an error.
///
/// If the promise object is reclaimed by the garbage collector without being
/// resolved or rejected, then the resulting future's `poll` will return an
/// error of kind `ErrorKind::JavaScriptPromiseCollectedWithoutSettling`.
pub fn promise_to_future<T, E>(promise: &GcRoot<*mut jsapi::JSObject>) -> Promise2Future<T, E>
where
    T: 'static + FromJSValConvertible<Config = ()>,
    E: 'static + FromJSValConvertible<Config = ()>,
{
    unsafe {
        let cx = JsRuntime::get();

        let (resolve_sender, resolve_receiver) = oneshot::channel();
        let on_resolve = make_js_fn(move |cx, args| {
            match T::from_jsval(cx, args.get(0), ()) {
                Err(()) => {
                    let err = Error::from_cx(cx);
                    let _ = resolve_sender.send(Err(err));
                }
                Ok(ConversionResult::Failure(s)) => {
                    let err = Err(ErrorKind::Msg(s.to_string()).into());
                    let _ = resolve_sender.send(err);
                }
                Ok(ConversionResult::Success(t)) => {
                    let _ = resolve_sender.send(Ok(Ok(t)));
                }
            }
            true
        });

        let (reject_sender, reject_receiver) = oneshot::channel();
        let on_reject = make_js_fn(move |cx, args| {
            match E::from_jsval(cx, args.get(0), ()) {
                Err(()) => {
                    let err = Error::from_cx(cx);
                    let _ = reject_sender.send(Err(err));
                }
                Ok(ConversionResult::Failure(s)) => {
                    let err = Err(ErrorKind::Msg(s.to_string()).into());
                    let _ = reject_sender.send(err);
                }
                Ok(ConversionResult::Success(t)) => {
                    let _ = reject_sender.send(Ok(Err(t)));
                }
            }
            true
        });

        rooted!(in(cx) let promise = promise.raw());
        rooted!(in(cx) let on_resolve = on_resolve.raw());
        rooted!(in(cx) let on_reject = on_reject.raw());
        assert!(jsapi::JS::AddPromiseReactions(
            cx,
            promise.handle(),
            on_resolve.handle(),
            on_reject.handle()
        ));

        Promise2Future {
            inner: resolve_receiver.select(reject_receiver),
        }
    }
}
