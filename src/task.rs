//! Tasks are lightweight, isolated, pre-emptively scheduled JavaScript
//! execution threads.
//!
//! One of the most foundational properties of Starling is that no individual
//! misbehaving JavaScript task can bring down the whole system.
//!
//! Tasks are **pre-emptively scheduled**. If one task goes into an infinite
//! loop, or generally becomes CPU bound for a sustained period of time, other
//! tasks in the system are not starved of resources, and system latency does
//! not fall off a cliff.
//!
//! Tasks are **isolated** from each other and share no memory. They communicate
//! by **message passing** over readable and writable streams. This means that
//! if one task's local state is corrupted, it doesn't immediately infect all
//! other tasks' states that are communicating with it. The corrupted task can
//! be killed by its supervisor and then respawned with the last known good
//! state or with a clean slate.
//!
//! Tasks are **lightweight**. In order to facilitate let-it-fail error handling
//! coupled with supervision hierarchies, idle tasks have little memory
//! overhead, and near no time overhead. Note that this is *aspirational* at the
//! moment: there is [ongoing work in SpiderMonkey][ongoing] to fully unlock
//! this.
//!
//! [ongoing]: https://bugzilla.mozilla.org/show_bug.cgi?id=1323066

use super::{Error, ErrorKind, FromPendingJsapiException, Result, StarlingHandle, StarlingMessage};
use future_ext::FutureExt;
use futures::{self, Async, Future, Sink, Stream};
use futures::sync::mpsc;
use futures::sync::oneshot;
use futures_cpupool::CpuFuture;
use gc_roots::{GcRoot, GcRootSet};
use js;
use js::heap::Trace;
use js::jsapi;
use js::jsval;
use js::rust::Runtime as JsRuntime;
use js_global::GLOBAL_FUNCTIONS;
use promise_future_glue::{future_to_promise, promise_to_future, Promise2Future};
use promise_tracker::RejectedPromisesTracker;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::os;
use std::path;
use std::ptr;
use std::thread;
use tokio_core;
use tokio_core::reactor::Core as TokioCore;

/// A unique identifier for some `Task`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskId(thread::ThreadId);

impl From<TaskId> for thread::ThreadId {
    fn from(task_id: TaskId) -> Self {
        task_id.0
    }
}

thread_local! {
    static THIS_JS_FILE: RefCell<Option<path::PathBuf>> = RefCell::new(None);
    static STARLING: RefCell<Option<StarlingHandle>> = RefCell::new(None);
    static THIS_TASK: RefCell<Option<TaskHandle>> = RefCell::new(None);
    static REJECTED_PROMISES: RefCell<RejectedPromisesTracker> = RefCell::new(Default::default());

    static EVENT_LOOP: RefCell<Option<tokio_core::reactor::Handle>> = RefCell::new(None);

    static CHILDREN: RefCell<HashMap<
        TaskId,
        (TaskHandle, oneshot::Sender<Result<()>>)
    >> = {
        RefCell::new(HashMap::new())
    };
}

/// Get a handle to this thread's task's starling supervisor.
pub(crate) fn starling_handle() -> StarlingHandle {
    STARLING.with(|starling| {
        starling
            .borrow()
            .clone()
            .expect("called `task::starling_handle` before creating thread's task")
    })
}

/// Get a handle to this thread's task.
pub(crate) fn this_task() -> TaskHandle {
    THIS_TASK.with(|this_task| {
        this_task
            .borrow()
            .clone()
            .expect("called `task::this_task` before creating thread's task")
    })
}

/// Get the JS file that this thread's task was spawned to evaluate.
pub(crate) fn this_js_file() -> path::PathBuf {
    THIS_JS_FILE.with(|this_js_file| {
        this_js_file
            .borrow()
            .clone()
            .expect("called `task::this_js_file` before creating thread's task")
    })
}

/// Get a handle to this thread's `tokio` event loop.
pub fn event_loop() -> tokio_core::reactor::Handle {
    EVENT_LOOP.with(|el| {
        el.borrow()
            .clone()
            .expect("called `task::event_loop` before initializing thread's event loop")
    })
}

/// Check for unhandled, rejected promises and return an error if any exist.
pub(crate) fn check_for_unhandled_rejected_promises() -> Result<()> {
    if let Some(unhandled) = REJECTED_PROMISES.with(|tracker| tracker.borrow_mut().take()) {
        Err(unhandled.into())
    } else {
        Ok(())
    }
}

/// Drain the micro-task queue and then check for unhandled rejected
/// promises. It is the caller's responsibility to ensure that any error
/// propagates along the task hierarchy correctly.
pub(crate) fn drain_micro_task_queue() -> Result<()> {
    let cx = JsRuntime::get();
    unsafe {
        jsapi::js::RunJobs(cx);
    }
    check_for_unhandled_rejected_promises()
}

/// A `Task` is a JavaScript execution thread.
///
/// A `Task` is not `Send` nor `Sync`; it must be communicated with via its
/// `TaskHandle` which is both `Send` and `Sync`.
///
/// See the module level documentation for details.
pub(crate) struct Task {
    handle: TaskHandle,
    receiver: mpsc::Receiver<TaskMessage>,
    global: js::heap::Heap<*mut jsapi::JSObject>,
    runtime: Option<JsRuntime>,
    starling: StarlingHandle,
    js_file: path::PathBuf,
    parent: Option<TaskHandle>,
    state: State,
}

impl Drop for Task {
    fn drop(&mut self) {
        unsafe {
            jsapi::JS_LeaveCompartment(self.runtime().cx(), ptr::null_mut());
        }

        self.global.set(ptr::null_mut());
        REJECTED_PROMISES.with(|rejected_promises| {
            rejected_promises.borrow_mut().clear();
        });
        GcRootSet::uninitialize();

        unsafe {
            jsapi::JS_RemoveExtraGCRootsTracer(
                self.runtime().cx(),
                Some(Self::trace_task_gc_roots),
                self as *const _ as *mut _,
            );
        }

        let _ = self.runtime.take().unwrap();
    }
}

/// ### Constructors
///
/// There are two kinds of tasks: the main task and child tasks. There is only
/// one main task, and there may be many child tasks. Every child task has a
/// parent whose lifetime strictly encapsulates the child's lifetime. The main
/// task's lifetime encapsulates the whole system's lifetime.
///
/// There are two different constructors for the two different kinds of tasks.
impl Task {
    /// Spawn the main task in a new native thread.
    ///
    /// The lifetime of the Starling system is tied to this task. If it exits
    /// successfully, the process will exit 0. If it fails, then the process
    /// will exit non-zero.
    pub fn spawn_main(
        starling: StarlingHandle,
        js_file: path::PathBuf,
    ) -> Result<(TaskHandle, thread::JoinHandle<()>)> {
        let (send_handle, recv_handle) = oneshot::channel();
        let starling2 = starling.clone();

        let join_handle = thread::spawn(move || {
            let result = TokioCore::new()
                .map_err(|e| e.into())
                .and_then(|event_loop| {
                    EVENT_LOOP.with(|el| {
                        let mut el = el.borrow_mut();
                        assert!(el.is_none());
                        *el = Some(event_loop.handle());
                    });

                    Self::create_main(starling2, js_file).map(|task| (event_loop, task))
                });

            let (mut event_loop, task) = match result {
                Ok(t) => t,
                Err(e) => {
                    send_handle
                        .send(Err(e))
                        .expect("Receiver half of the oneshot should not be dropped");
                    return;
                }
            };

            let id = task.handle().id();

            send_handle
                .send(Ok(task.handle()))
                .expect("Receiver half of the oneshot should not be dropped");

            if let Err(e) = event_loop.run(task) {
                // OK to wait synchronously and maybe panic here because this is
                // pretty last ditch: all the recoverable errors are handled in
                // `impl Future for Task`.
                starling
                    .send(StarlingMessage::TaskErrored(id, e))
                    .wait()
                    .expect("couldn't send task error to starling supervisory thread");
            }
        });

        let task_handle = recv_handle
            .wait()
            .expect("Sender half of the oneshot should never cancel")?;

        Ok((task_handle, join_handle))
    }

    /// Create a new child task.
    ///
    /// The child task's lifetime is constrained within its parent task's
    /// lifetime. When the parent task terminates, the child is terminated as
    /// well.
    ///
    /// May only be called from a parent task.
    ///
    /// Returns a Promise object that is resolved or rejected when the child
    /// task finishes cleanly or with an error respectively.
    pub fn spawn_child(
        starling: StarlingHandle,
        js_file: path::PathBuf
    ) -> Result<GcRoot<*mut jsapi::JSObject>> {
        let parent_task = this_task();
        let (send_handle, recv_handle) = oneshot::channel();
        let starling2 = starling.clone();

        let join_handle = thread::spawn(move || {
            let result = TokioCore::new()
                .map_err(|e| e.into())
                .and_then(|event_loop| {
                    EVENT_LOOP.with(|el| {
                        let mut el = el.borrow_mut();
                        assert!(el.is_none());
                        *el = Some(event_loop.handle());
                    });

                    Self::create_child(starling2.clone(), parent_task, js_file)
                        .map(|task| (event_loop, task))
                });

            let (mut event_loop, task) = match result {
                Ok(t) => t,
                Err(e) => {
                    send_handle
                        .send(Err(e))
                        .expect("Receiver half of the oneshot should not be dropped");
                    return;
                }
            };

            let id = task.handle().id();

            send_handle
                .send(Ok(task.handle()))
                .expect("Receiver half of the oneshot should not be dropped");

            if let Err(e) = event_loop.run(task) {
                // OK to wait synchronously and maybe panic here because this is
                // pretty last ditch: all the recoverable errors are handled in
                // `impl Future for Task`.
                starling2
                    .send(StarlingMessage::TaskErrored(id, e))
                    .wait()
                    .expect("couldn't send task error to starling supervisory thread");
            }
        });

        let task_handle = recv_handle
            .wait()
            .expect("Sender half of the oneshot should never cancel")?;

        starling.send(StarlingMessage::NewTask(task_handle.clone(), join_handle))
            .wait()
            .expect("should never outlive supervisor");

        let (sender, receiver) = oneshot::channel();

        CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            children.insert(task_handle.id(), (task_handle, sender));
        });

        Ok(future_to_promise(
            receiver
                .map_err(|e| e.to_string())
                .flatten()
                .map_err(|e| e.to_string())
        ))
    }

    fn create_main(starling: StarlingHandle, js_file: path::PathBuf) -> Result<Box<Task>> {
        Self::create(starling, None, js_file)
    }

    fn create_child(
        starling: StarlingHandle,
        parent: TaskHandle,
        js_file: path::PathBuf
    ) -> Result<Box<Task>> {
        Self::create(starling, Some(parent), js_file)
    }

    fn create(
        starling: StarlingHandle,
        parent: Option<TaskHandle>,
        js_file: path::PathBuf,
    ) -> Result<Box<Task>> {
        let runtime = Some(JsRuntime::new(true).map_err(|_| {
            Error::from_kind(ErrorKind::CouldNotCreateJavaScriptRuntime)
        })?);

        let capacity = starling.options().buffer_capacity_for::<TaskMessage>();
        let (sender, receiver) = mpsc::channel(capacity);

        let id = TaskId(thread::current().id());
        let handle = TaskHandle {
            id,
            sender,
        };

        THIS_JS_FILE.with(|this_js_file| {
            let mut this_js_file = this_js_file.borrow_mut();
            assert!(this_js_file.is_none());
            *this_js_file = Some(js_file.clone());
        });

        THIS_TASK.with(|this_task| {
            let mut this_task = this_task.borrow_mut();
            assert!(this_task.is_none());
            *this_task = Some(handle.clone());
        });

        STARLING.with(|starling_handle| {
            let mut starling_handle = starling_handle.borrow_mut();
            assert!(starling_handle.is_none());
            *starling_handle = Some(starling.clone());
        });

        let task = Box::new(Task {
            handle,
            receiver,
            global: js::heap::Heap::default(),
            runtime,
            starling,
            js_file,
            parent,
            state: State::Created,
        });

        GcRootSet::initialize();
        REJECTED_PROMISES.with(|rejected_promises| {
            RejectedPromisesTracker::register(task.runtime(), &rejected_promises);
        });
        task.create_global();

        Ok(task)
    }
}

impl Task {
    /// Get a handle to this task.
    pub fn handle(&self) -> TaskHandle {
        self.handle.clone()
    }

    fn id(&self) -> TaskId {
        self.handle.id
    }

    fn runtime(&self) -> &JsRuntime {
        self.runtime
            .as_ref()
            .expect("Task should always have a JS runtime except at the very end of its Drop")
    }

    fn create_global(&self) {
        assert_eq!(self.global.get(), ptr::null_mut());

        unsafe {
            let cx = self.runtime().cx();

            rooted!(in(cx) let global = jsapi::JS_NewGlobalObject(
                cx,
                &js::rust::SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                jsapi::JS::OnNewGlobalHookOption::FireOnNewGlobalHook,
                &jsapi::JS::CompartmentOptions::default()
            ));
            assert!(!global.get().is_null());
            self.global.set(global.get());
            jsapi::JS_EnterCompartment(cx, self.global.get());

            js::rust::define_methods(cx, global.handle(), &GLOBAL_FUNCTIONS[..])
                .expect("should define global functions OK");

            assert!(jsapi::JS_AddExtraGCRootsTracer(
                cx,
                Some(Self::trace_task_gc_roots),
                self as *const Task as *mut os::raw::c_void
            ));
        }
    }

    /// Notify the SpiderMonkey GC of our additional GC roots.
    unsafe extern "C" fn trace_task_gc_roots(
        tracer: *mut jsapi::JSTracer,
        task: *mut os::raw::c_void,
    ) {
        let task = task as *const os::raw::c_void;
        let task = task as *const Task;
        let task = task.as_ref().unwrap();
        task.trace(tracer);
    }
}

unsafe impl Trace for Task {
    unsafe fn trace(&self, tracer: *mut jsapi::JSTracer) {
        self.global.trace(tracer);

        GcRootSet::with_ref(|roots| {
            roots.trace(tracer);
        });
    }
}

/// State transition helper methods called from `Future::poll`.
///
/// In general, these methods need to re-call `poll()` so that the newly
/// transitioned-to state's new future gets registered with the `tokio` reactor
/// core. If we don't, then we'll dead lock.
impl Task {
    fn read_js_module(&mut self) -> Result<Async<()>> {
        assert!(self.state.is_created());

        let js_file_path = self.js_file.clone();

        let reading = self.starling.sync_io_pool().spawn_fn(|| {
            use std::fs;
            use std::io::Read;

            let mut file = fs::File::open(js_file_path)?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            Ok(contents)
        });

        self.state = State::ReadingJsModule(reading);
        self.poll()
    }

    fn evaluate_top_level(&mut self, src: String) -> Result<Async<()>> {
        let cx = self.runtime().cx();
        rooted!(in(cx) let global = self.global.get());

        let filename = self.js_file.display().to_string();

        // Evaluate the JS source.

        rooted!(in(cx) let mut rval = jsval::UndefinedValue());
        let eval_result =
            self.runtime()
                .evaluate_script(global.handle(), &src, &filename, 1, rval.handle_mut());
        if let Err(()) = eval_result {
            unsafe {
                let err = Error::take_pending(cx)
                    .expect("evaluate_script shouldn't return an Err without a \
                             pending exception");
                jsapi::js::RunJobs(cx);
                return self.notify_starling_errored(err);
            }
        }

        if let Err(e) = drain_micro_task_queue() {
            return self.notify_starling_errored(e);
        }

        self.evaluate_main()
    }

    fn evaluate_main(&mut self) -> Result<Async<()>> {
        let cx = self.runtime().cx();
        rooted!(in(cx) let global = self.global.get());

        let mut has_main = false;
        unsafe {
            assert!(jsapi::JS_HasProperty(
                cx,
                global.handle(),
                b"main\0".as_ptr() as _,
                &mut has_main
            ));
        }
        if !has_main {
            return self.notify_starling_finished();
        }

        rooted!(in(cx) let mut rval = jsval::UndefinedValue());
        let args = jsapi::JS::HandleValueArray::new();
        unsafe {
            let ok = jsapi::JS_CallFunctionName(
                cx,
                global.handle(),
                b"main\0".as_ptr() as _,
                &args,
                rval.handle_mut()
            );

            if !ok {
                let err = Error::take_pending(cx)
                    .expect("JS_CallFunctionName should not return false without \
                             setting a pending exception");

                // TODO: It isn't obvious that we should drain the micro-task
                // queue here. But it also isn't obvious that we shouldn't.
                // Let's investigate this sometime in the future.
                jsapi::js::RunJobs(cx);
                return self.notify_starling_errored(err);
            }
        }

        rooted!(in(cx) let mut obj = ptr::null_mut());

        let mut is_promise = false;
        if rval.get().is_object() {
            obj.set(rval.get().to_object());
            is_promise = unsafe {
                jsapi::JS::IsPromiseObject(obj.handle())
            };
        }
        if is_promise {
            let obj = GcRoot::new(obj.get());
            let future = promise_to_future(&obj);
            self.state = State::WaitingOnPromise(future);

            if let Err(e) = drain_micro_task_queue() {
                return self.notify_starling_errored(e);
            }

            self.poll()
        } else {
            if let Err(e) = drain_micro_task_queue() {
                return self.notify_starling_errored(e);
            }

            self.notify_starling_finished()
        }
    }

    fn handle_child_finished(&mut self, id: TaskId) -> Result<Async<()>> {
        assert!(self.state.is_waiting_on_promise());
        let mut error = None;

        CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            if let Some((_, sender)) = children.remove(&id) {
                // The receiver half could be dropped if the promise is GC'd, so
                // ignore any `send` errors.
                let _ = sender.send(Ok(()));
            } else {
                let msg = format!(
                    "task received message for child that isn't \
                     actually a child: {:?}",
                    id
                );
                error = Some(msg.into());
            }
        });

        if let Some(e) = error {
            return self.notify_starling_errored(e);
        }

        // Make sure to run JS outside of the `CHILDREN.with` closure, since JS
        // could spawn a new task, causing us to re-enter `CHILDREN` and panic.
        if let Err(e) = drain_micro_task_queue() {
            self.notify_starling_errored(e)
        } else {
            self.poll()
        }
    }

    fn handle_child_errored(&mut self, id: TaskId, err: Error) -> Result<Async<()>> {
        assert!(self.state.is_waiting_on_promise());
        let mut error = None;

        CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            if let Some((_, sender)) = children.remove(&id) {
                // Again, the receiver half could have been GC'd, causing the
                // oneshot `send` to fail. If that is the case, then we
                // propagate the error to the parent.
                if sender.send(Err(err.clone())).is_err() {
                    error = Some(err);
                }
            } else {
                let msg = format!(
                    "task received message for child that isn't \
                     actually a child: {:?}",
                    id
                );
                error = Some(msg.into());
            }
        });

        if let Some(e) = error {
            return self.notify_starling_errored(e);
        }

        // As in `handle_child_finished`, we take care that we don't run JS
        // inside the `CHILDREN` block.
        if let Err(e) = drain_micro_task_queue() {
            self.notify_starling_errored(e)
        } else {
            self.poll()
        }
    }

    fn notify_starling_finished(&mut self) -> Result<Async<()>> {
        let notify = self.starling.send(StarlingMessage::TaskFinished(self.id()));
        self.state = State::NotifyStarlingFinished(notify);
        self.poll()
    }

    fn notify_starling_errored(&mut self, error: Error) -> Result<Async<()>> {
        let notify = self.starling
            .send(StarlingMessage::TaskErrored(self.id(), error.clone()));
        self.state = State::NotifyStarlingErrored(error, notify);
        self.poll()
    }

    fn notify_parent_finished(&mut self) -> Result<Async<()>> {
        assert!(self.state.is_notify_starling_finished());
        if let Some(parent) = self.parent.clone() {
            let notify = parent.send(TaskMessage::ChildTaskFinished(self.id()));
            self.state = State::NotifyParentFinished(notify);
            self.poll()
        } else {
            self.shutdown_children()
        }
    }

    fn notify_parent_errored(&mut self, error: Error) -> Result<Async<()>> {
        assert!(self.state.is_notify_starling_errored());
        if let Some(parent) = self.parent.clone() {
            let notify = parent.send(TaskMessage::ChildTaskErrored(self.id(), error));
            self.state = State::NotifyParentErrored(notify);
            self.poll()
        } else {
            self.shutdown_children()
        }
    }

    fn shutdown_children(&mut self) -> Result<Async<()>> {
        assert!(
            self.state.is_notify_starling_finished() ||
                self.state.is_notify_parent_finished() ||
                self.state.is_notify_starling_errored() ||
                self.state.is_notify_parent_errored()
        );

        let mut shutdown: Box<Future<Item = (), Error = Error>> =
            Box::new(futures::future::ok(()));

        let shutdown = CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            for (_, (child, _)) in children.drain() {
                shutdown = Box::new(
                    shutdown
                        .join(
                            child
                                .send(TaskMessage::Shutdown)
                                .map_err(|_| "could not send shutdown notice to child".into())
                        )
                        .map(|_| ())
                );
            }

            shutdown
        });

        self.state = State::ShutdownChildren(shutdown);
        self.poll()
    }
}

#[derive(is_enum_variant)]
enum State {
    Created,
    ReadingJsModule(CpuFuture<String, Error>),
    WaitingOnPromise(Promise2Future<GcRoot<jsapi::JS::Value>, GcRoot<jsapi::JS::Value>>),

    NotifyStarlingFinished(futures::sink::Send<mpsc::Sender<StarlingMessage>>),
    NotifyParentFinished(futures::sink::Send<mpsc::Sender<TaskMessage>>),

    NotifyStarlingErrored(Error, futures::sink::Send<mpsc::Sender<StarlingMessage>>),
    NotifyParentErrored(futures::sink::Send<mpsc::Sender<TaskMessage>>),

    ShutdownChildren(Box<Future<Item = (), Error = Error>>),
}

enum NextState {
    EvaluateTopLevel(String),
    HandleChildFinished(TaskId),
    HandleChildErrored(TaskId, Error),
    NotifyStarlingFinished,
    NotifyStarlingErrored(Error),
    NotifyParentFinished,
    NotifyParentErrored(Error),
    ShutdownChildren,
}

impl Future for Task {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>> {
        // Principles for error handling, recovery, and propagation when driving
        // tasks to completion:
        //
        // * Whenever possible, catch errors and then inform
        //     1. the Starling system supervisor thread,
        //     2. the parent task (if any),
        //     3. and this task's children
        //   in that order to propagate them.
        //
        // * Tasks should never outlive the Starling system supervisor thread,
        //   so don't attempt to catch errors sending messages to it. Just panic
        //   if that fails, since something very wrong has happened.
        //
        // * We can't rely on this task's children or parent stricly observing
        //   the tree lifetime discipline that we expose to JavaScript. There
        //   can be races where both shut down at the same time, perhaps
        //   involving the main task returning and the Starling system
        //   supervisor thread sending shutdown notices to all remaining
        //   tasks. Therefore, we allow all inter-task messages to fail
        //   silently, locally ignoring errors, and relying on the Starling
        //   system supervisor thread to perform ultimate clean up.

        let next_state = match self.state {
            State::Created => {
                return self.read_js_module();
            }
            State::ReadingJsModule(ref mut reading) => {
                match reading.poll() {
                    Err(e) => NextState::NotifyStarlingErrored(e),
                    Ok(Async::NotReady) => return Ok(Async::NotReady),
                    Ok(Async::Ready(src)) => NextState::EvaluateTopLevel(src),
                }
            }
            State::WaitingOnPromise(ref mut promise) => {
                // The task's `async function main` is waiting on a promise, but
                // we need to also listen for messages from our parent, the
                // starling supervisor thread, or our children. These messages
                // might settle this promise or other promises that trigger a
                // chain of events that eventually settle the promise our task
                // is waiting on. Therefore, we need to select from either the
                // promise we're waiting on or our channel, and depending which
                // wins the race we need to handle it differently, so we have
                // this little type to let us know which won the race.
                enum Which<T, E> {
                    PromiseSettled(::std::result::Result<T, E>),
                    ChannelClosed,
                    Msg(TaskMessage),
                }

                // The next message from this task's channel.
                let next_msg = self.receiver
                    .by_ref()
                    .take(1)
                    .into_future()
                    .map(|(item, _rest)| match item {
                        None => Which::ChannelClosed,
                        Some(msg) => Which::Msg(msg),
                    })
                    .map_err(|_| {
                        ErrorKind::CouldNotReadValueFromChannel.into()
                    });

                // The promise this task is waiting on.
                let promise = promise.map(|p| Which::PromiseSettled(p));

                // Race between the promise and the next channel message. Since
                // we only care about which one wins the race, and don't plan on
                // eventually running both to completion here (that will happen
                // on the next `poll` where we are still in the
                // `WaitingOnPromise` state when we recombine the futures), we
                // ignore the `_next` parameter.
                let mut promise_or_next_msg = promise.select(next_msg)
                    .map(|(x, _next)| x)
                    .map_err(|(e, _next)| e);

                match promise_or_next_msg.poll() {
                    Err(err) => {
                        NextState::NotifyStarlingErrored(err)
                    }
                    Ok(Async::NotReady) => {
                        return Ok(Async::NotReady);
                    }
                    Ok(Async::Ready(Which::PromiseSettled(Err(val)))) => {
                        let cx = JsRuntime::get();
                        unsafe {
                            rooted!(in(cx) let val = val.raw());
                            let err = Error::infallible_from_jsval(cx, val.handle());
                            NextState::NotifyStarlingErrored(err)
                        }
                    }
                    Ok(Async::Ready(Which::PromiseSettled(Ok(_)))) => {
                        NextState::NotifyStarlingFinished
                    }
                    Ok(Async::Ready(Which::ChannelClosed)) => {
                        let err = ErrorKind::CouldNotReadValueFromChannel.into();
                        NextState::NotifyStarlingErrored(err)
                    }
                    Ok(Async::Ready(Which::Msg(TaskMessage::Shutdown))) => {
                        NextState::NotifyStarlingFinished
                    }
                    Ok(Async::Ready(Which::Msg(TaskMessage::ChildTaskFinished(id)))) => {
                        NextState::HandleChildFinished(id)
                    }
                    Ok(Async::Ready(Which::Msg(TaskMessage::ChildTaskErrored(id, err)))) => {
                        NextState::HandleChildErrored(id, err)
                    }
                    Ok(Async::Ready(Which::Msg(TaskMessage::UnhandledRejectedPromise(err)))) => {
                        NextState::NotifyStarlingErrored(err)
                    }
                }
            }
            State::NotifyStarlingFinished(ref mut notify) => {
                try_ready!(
                    notify
                        .expect::<Error>("task should never outlive supervisor")
                        .poll()
                );
                NextState::NotifyParentFinished
            }
            State::NotifyParentFinished(ref mut notify) => {
                try_ready!(
                    notify
                        .ignore_results::<Error>()
                        .poll()
                );
                NextState::ShutdownChildren
            }
            State::NotifyStarlingErrored(ref error, ref mut notify) => {
                try_ready!(
                    notify
                        .expect::<Error>("task should never outlive supervisor")
                        .poll()
                );
                NextState::NotifyParentErrored(error.clone())
            }
            State::NotifyParentErrored(ref mut notify) => {
                try_ready!(
                    notify
                        .ignore_results::<Error>()
                        .poll()
                );
                NextState::ShutdownChildren
            }
            State::ShutdownChildren(ref mut shutdown) => {
                try_ready!(
                    shutdown
                        .ignore_results::<Error>()
                        .poll()
                );
                return Ok(Async::Ready(()));
            }
        };

        match next_state {
            NextState::EvaluateTopLevel(src) => self.evaluate_top_level(src),
            NextState::HandleChildFinished(id) => self.handle_child_finished(id),
            NextState::HandleChildErrored(id, err) => self.handle_child_errored(id, err),
            NextState::NotifyStarlingFinished => self.notify_starling_finished(),
            NextState::NotifyStarlingErrored(e) => self.notify_starling_errored(e),
            NextState::NotifyParentFinished => self.notify_parent_finished(),
            NextState::NotifyParentErrored(e) => self.notify_parent_errored(e),
            NextState::ShutdownChildren => self.shutdown_children(),
        }
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Task {{ .. }}")
    }
}

/// A non-owning handle to a task.
///
/// A `TaskHandle` grants the ability to communicate with its referenced task
/// and send it `TaskMessage`s.
///
/// A `TaskHandle` does not keep its referenced task running. For example, if
/// the task's `main` function returns, or its parent terminates, or it stops
/// running for any other reason, then any further messages sent to the task
/// from the handle will return `Err`s.
#[derive(Clone)]
pub(crate) struct TaskHandle {
    id: TaskId,
    sender: mpsc::Sender<TaskMessage>,
}

impl TaskHandle {
    /// Get this task's ID.
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Send a message to the task.
    pub fn send(&self, msg: TaskMessage) -> futures::sink::Send<mpsc::Sender<TaskMessage>> {
        self.sender.clone().send(msg)
    }
}

impl fmt::Debug for TaskHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TaskHandle {{ {:?} }}", self.id)
    }
}

/// Messages that can be sent to a task.
#[derive(Debug)]
pub(crate) enum TaskMessage {
    Shutdown,
    ChildTaskFinished(TaskId),
    ChildTaskErrored(TaskId, Error),
    UnhandledRejectedPromise(Error),
}
