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
use future_ext::{FutureExt, ready};
use futures::{self, Async, Future, Poll, Sink, Stream};
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
use state_machine_future::RentToOwn;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::os;
use std::path;
use std::ptr;
use std::thread;
use tokio_core;
use tokio_core::reactor::Core as TokioCore;
use void::Void;

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
///
/// This needs to be `pub` because it is used in a trait implementation; don't
/// actually use it!
#[doc(hidden)]
pub struct Task {
    handle: TaskHandle,
    receiver: mpsc::Receiver<TaskMessage>,
    global: js::heap::Heap<*mut jsapi::JSObject>,
    runtime: Option<JsRuntime>,
    starling: StarlingHandle,
    js_file: path::PathBuf,
    parent: Option<TaskHandle>,
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
    pub(crate) fn spawn_main(
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

            send_handle
                .send(Ok(task.handle()))
                .expect("Receiver half of the oneshot should not be dropped");

            if event_loop.run(TaskStateMachine::start(task)).is_err() {
                // Because our error type, `Void`, is uninhabited -- indeed,
                // that is its sole reason for existence -- the only way we
                // could get here is if someone transmuted a `Void` out of thin
                // air. That is, hopefully obviously, not a good idea.
                unreachable!();
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
    pub(crate) fn spawn_child(
        starling: StarlingHandle,
        js_file: path::PathBuf,
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

            send_handle
                .send(Ok(task.handle()))
                .expect("Receiver half of the oneshot should not be dropped");

            if event_loop.run(TaskStateMachine::start(task)).is_err() {
                // Same deal as in `spawn_main`.
                unreachable!();
            }
        });

        let task_handle = recv_handle
            .wait()
            .expect("Sender half of the oneshot should never cancel")?;

        starling
            .send(StarlingMessage::NewTask(task_handle.clone(), join_handle))
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
                .map_err(|e| e.to_string()),
        ))
    }

    fn create_main(starling: StarlingHandle, js_file: path::PathBuf) -> Result<Box<Task>> {
        Self::create(starling, None, js_file)
    }

    fn create_child(
        starling: StarlingHandle,
        parent: TaskHandle,
        js_file: path::PathBuf,
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
        let handle = TaskHandle { id, sender };

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
    pub(crate) fn handle(&self) -> TaskHandle {
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

    fn evaluate_top_level(&mut self, src: String) -> Result<Option<Promise2FutureJsValue>> {
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
                let error = Error::from_cx(cx);
                jsapi::js::RunJobs(cx);
                return Err(error);
            }
        }

        drain_micro_task_queue()?;
        self.evaluate_main()
    }

    fn evaluate_main(&mut self) -> Result<Option<Promise2FutureJsValue>> {
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
            return Ok(None);
        }

        rooted!(in(cx) let mut rval = jsval::UndefinedValue());
        let args = jsapi::JS::HandleValueArray::new();
        unsafe {
            let ok = jsapi::JS_CallFunctionName(
                cx,
                global.handle(),
                b"main\0".as_ptr() as _,
                &args,
                rval.handle_mut(),
            );

            if !ok {
                let error = Error::from_cx(cx);

                // TODO: It isn't obvious that we should drain the micro-task
                // queue here. But it also isn't obvious that we shouldn't.
                // Let's investigate this sometime in the future.
                jsapi::js::RunJobs(cx);

                return Err(error);
            }
        }

        rooted!(in(cx) let mut obj = ptr::null_mut());

        let mut is_promise = false;
        if rval.get().is_object() {
            obj.set(rval.get().to_object());
            is_promise = unsafe { jsapi::JS::IsPromiseObject(obj.handle()) };
        }
        if is_promise {
            let obj = GcRoot::new(obj.get());
            let future = promise_to_future(&obj);
            drain_micro_task_queue()?;
            Ok(Some(future))
        } else {
            drain_micro_task_queue()?;
            Ok(None)
        }
    }

    fn shutdown_children(&self) -> Box<Future<Item = (), Error = Error>> {
        let mut shutdowns_sent: Box<Future<Item = (), Error = Error>> =
            Box::new(futures::future::ok(()));

        CHILDREN.with(|children| {
            let mut children = children.borrow_mut();
            for (_, (child, _)) in children.drain() {
                shutdowns_sent = Box::new(
                    shutdowns_sent
                        .join(
                            child
                                .send(TaskMessage::Shutdown)
                                .map_err(|_| "could not send shutdown notice to child".into()),
                        )
                        .map(|_| ()),
                );
            }

            shutdowns_sent
        })
    }

    #[inline]
    fn propagate<T>(self: Box<Self>, error: Error) -> Poll<T, Void>
    where
        T: From<ShutdownChildrenErrored>
    {
        let shutdowns_sent = self.shutdown_children();
        ready(ShutdownChildrenErrored {
            task: self,
            error,
            shutdowns_sent,
        })
    }

    #[inline]
    fn finished<T>(self: Box<Self>) -> Poll<T, Void>
    where
        T: From<ShutdownChildrenFinished>
    {
        let shutdowns_sent = self.shutdown_children();
        ready(ShutdownChildrenFinished {
            task: self,
            shutdowns_sent,
        })
    }

    fn handle_child_finished(&mut self, id: TaskId) -> Result<()> {
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
            return Err(e);
        }

        // Make sure to run JS outside of the `CHILDREN.with` closure, since JS
        // could spawn a new task, causing us to re-enter `CHILDREN` and panic.
        drain_micro_task_queue()
    }

    fn handle_child_errored(&mut self, id: TaskId, err: Error) -> Result<()> {
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
            return Err(e);
        }

        // As in `handle_child_finished`, we take care that we don't run JS
        // inside the `CHILDREN` block.
        drain_micro_task_queue()
    }

    fn notify_starling_finished(self: Box<Self>) -> NotifyStarlingFinished {
        let notification = self.starling.send(StarlingMessage::TaskFinished(self.id()));
        NotifyStarlingFinished {
            task: self,
            notification,
        }
    }

    fn notify_starling_errored(self: Box<Self>, error: Error) -> NotifyStarlingErrored {
        let notification = self.starling.send(StarlingMessage::TaskErrored(self.id(), error));
        NotifyStarlingErrored {
            task: self,
            notification,
        }
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

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Task {{ .. }}")
    }
}

type Promise2FutureJsValue = Promise2Future<GcRoot<jsapi::JS::Value>, GcRoot<jsapi::JS::Value>>;

#[derive(StateMachineFuture)]
#[allow(dead_code)]
enum TaskStateMachine {
    /// The initial state where the task is created but has not evaluated any JS
    /// yet.
    #[state_machine_future(start, transitions(ReadingJsModule))]
    Created { task: Box<Task> },

    /// We are waiting on reading the JS source text from disk.
    #[state_machine_future(transitions(WaitingOnPromise, ShutdownChildrenFinished,
                                       ShutdownChildrenErrored))]
    ReadingJsModule {
        task: Box<Task>,
        reading: CpuFuture<String, Error>,
    },

    /// The most common state: waiting on the task's `main`'s result promise to
    /// complete, and handling incoming messages while we do so.
    #[state_machine_future(transitions(WaitingOnPromise, ShutdownChildrenFinished,
                                       ShutdownChildrenErrored))]
    WaitingOnPromise {
        task: Box<Task>,
        promise: Promise2FutureJsValue,
    },

    /// The task has finished OK, and now we need to shutdown its children.
    #[state_machine_future(transitions(NotifyParentFinished, NotifyStarlingFinished))]
    ShutdownChildrenFinished {
        task: Box<Task>,
        shutdowns_sent: Box<Future<Item = (), Error = Error>>,
    },

    /// The task has finished OK, and now we need to notify the parent that it
    /// is shutting down.
    #[state_machine_future(transitions(NotifyStarlingFinished))]
    NotifyParentFinished {
        task: Box<Task>,
        notification: futures::sink::Send<mpsc::Sender<TaskMessage>>,
    },

    /// The task has finished OK, and now we need to notify the Starling
    /// supervisor that it is shutting down.
    #[state_machine_future(transitions(Finished))]
    NotifyStarlingFinished {
        task: Box<Task>,
        notification: futures::sink::Send<mpsc::Sender<StarlingMessage>>,
    },

    /// The task errored out, and now we need to clean up its children.
    #[state_machine_future(transitions(NotifyParentErrored, NotifyStarlingErrored))]
    ShutdownChildrenErrored {
        task: Box<Task>,
        error: Error,
        shutdowns_sent: Box<Future<Item = (), Error = Error>>,
    },

    /// The task errored out, and now we need to notify its parent.
    #[state_machine_future(transitions(NotifyStarlingErrored))]
    NotifyParentErrored {
        task: Box<Task>,
        error: Error,
        notification: futures::sink::Send<mpsc::Sender<TaskMessage>>,
    },

    /// The task errored out, and now we need to notify the Starling supervisor.
    #[state_machine_future(transitions(Finished))]
    NotifyStarlingErrored {
        task: Box<Task>,
        notification: futures::sink::Send<mpsc::Sender<StarlingMessage>>,
    },

    /// The task completed (either OK or with an error) and we're done notifying
    /// the appropriate parties.
    #[state_machine_future(ready)]
    Finished(()),

    /// We handle and propagate all errors, and to enforce this at the type
    /// system level, our `Future::Error` type is the uninhabited `Void`.
    #[state_machine_future(error)]
    Impossible(Void),
}

/// Principles for error handling, recovery, and propagation when driving
/// tasks to completion:
///
/// * Whenever possible, catch errors and then inform
///     1. this task's children,
///     2. the parent task (if any), and
///     3. the Starling system supervisor thread
///   in that order to propagate the errors. This ordering is enforced by the
///   state machine transitions and generated typestates. All errors that we
///   can't reasonably catch-and-propagate should be `unwrap`ed.
///
/// * Tasks should never outlive the Starling system supervisor thread,
///   so don't attempt to catch errors sending messages to it. Just panic
///   if that fails, since something very wrong has happened.
///
/// * We can't rely on this task's children or parent strictly observing the
///   tree lifetime discipline that we expose to JavaScript. There can be races
///   where both shut down at the same time, perhaps because they both had
///   unhandled exceptions at the same time. Therefore, we allow all inter-task
///   messages to fail silently, locally ignoring errors, and relying on the
///   Starling system supervisor thread to perform ultimate clean up.
impl PollTaskStateMachine for TaskStateMachine {
    fn poll_created<'a>(created: &'a mut RentToOwn<'a, Created>) -> Poll<AfterCreated, Void> {
        let js_file_path = created.task.js_file.clone();

        let reading = created.task.starling.sync_io_pool().spawn_fn(|| {
            use std::fs;
            use std::io::Read;

            let mut file = fs::File::open(js_file_path)?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            Ok(contents)
        });

        let task = created.take().task;
        ready(ReadingJsModule {
            task,
            reading,
        })
    }

    fn poll_reading_js_module<'a>(
        reading: &'a mut RentToOwn<'a, ReadingJsModule>
    ) -> Poll<AfterReadingJsModule, Void> {
        let src = match reading.reading.poll() {
            Err(e) => return reading.take().task.propagate(e),
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Ok(Async::Ready(src)) => src,
        };

        match reading.task.evaluate_top_level(src) {
            Err(e) => reading.take().task.propagate(e),
            Ok(None) => reading.take().task.finished(),
            Ok(Some(promise)) => ready(WaitingOnPromise {
                task: reading.take().task,
                promise,
            })
        }
    }

    fn poll_waiting_on_promise<'a>(
        waiting: &'a mut RentToOwn<'a, WaitingOnPromise>
    ) -> Poll<AfterWaitingOnPromise, Void> {
        let next_msg = waiting.task.receiver
            .by_ref()
            .take(1)
            .into_future()
            .map(|(item, _)| item)
            .map_err(|_| ErrorKind::CouldNotReadValueFromChannel.into())
            .poll();
        match next_msg {
            Err(e) => return waiting.take().task.propagate(e),
            Ok(Async::Ready(None)) => {
                let error = ErrorKind::CouldNotReadValueFromChannel.into();
                return waiting.take().task.propagate(error);
            }
            Ok(Async::Ready(Some(TaskMessage::UnhandledRejectedPromise { error }))) => {
                return waiting.take().task.propagate(error);
            }
            Ok(Async::Ready(Some(TaskMessage::Shutdown))) => {
                waiting.task.parent = None;
                return waiting.take().task.finished();
            }
            Ok(Async::Ready(Some(TaskMessage::ChildTaskFinished { child }))) => {
                return match waiting.task.handle_child_finished(child) {
                    Err(e) => waiting.take().task.propagate(e),
                    Ok(()) => ready(waiting.take()),
                };
            }
            Ok(Async::Ready(Some(TaskMessage::ChildTaskErrored { child, error }))) => {
                return match waiting.task.handle_child_errored(child, error) {
                    Err(e) => waiting.take().task.propagate(e),
                    Ok(()) => ready(waiting.take()),
                };
            }
            Ok(Async::NotReady) => {}
        }

        match waiting.promise.poll() {
            Err(e) => {
                waiting.take().task.propagate(e)
            },
            Ok(Async::NotReady) => {
                Ok(Async::NotReady)
            }
            Ok(Async::Ready(Err(val))) => {
                let cx = waiting.task.runtime().cx();
                unsafe {
                    rooted!(in(cx) let val = val.raw());
                    let err = Error::infallible_from_jsval(cx, val.handle());
                    waiting.take().task.propagate(err)
                }
            }
            Ok(Async::Ready(Ok(_))) => {
                waiting.take().task.finished()
            }
        }
    }

    fn poll_shutdown_children_finished<'a>(
        shutdown: &'a mut RentToOwn<'a, ShutdownChildrenFinished>
    ) -> Poll<AfterShutdownChildrenFinished, Void> {
        try_ready!(shutdown.shutdowns_sent.by_ref().ignore_results::<Void>().poll());

        if let Some(parent) = shutdown.task.parent.clone() {
            let child = shutdown.task.id();
            ready(NotifyParentFinished {
                task: shutdown.take().task,
                notification: parent.send(TaskMessage::ChildTaskFinished { child }),
            })
        } else {
            ready(shutdown.take().task.notify_starling_finished())
        }
    }

    fn poll_notify_parent_finished<'a>(
        notify: &'a mut RentToOwn<'a, NotifyParentFinished>
    ) -> Poll<AfterNotifyParentFinished, Void> {
        try_ready!(notify.notification.by_ref().ignore_results::<Void>().poll());
        ready(notify.take().task.notify_starling_finished())
    }

    fn poll_notify_starling_finished<'a>(
        notify: &'a mut RentToOwn<'a, NotifyStarlingFinished>
    ) -> Poll<AfterNotifyStarlingFinished, Void> {
        try_ready!(notify.notification.by_ref().ignore_results::<Void>().poll());
        ready(Finished(()))
    }

    fn poll_shutdown_children_errored<'a>(
        shutdown: &'a mut RentToOwn<'a, ShutdownChildrenErrored>
    ) -> Poll<AfterShutdownChildrenErrored, Void> {
        try_ready!(shutdown.shutdowns_sent.by_ref().ignore_results::<Void>().poll());

        if let Some(parent) = shutdown.task.parent.clone() {
            let child = shutdown.task.id();
            let shutdown = shutdown.take();
            let error = shutdown.error.clone();
            ready(NotifyParentErrored {
                task: shutdown.task,
                error: shutdown.error,
                notification: parent.send(TaskMessage::ChildTaskErrored { child, error }),
            })
        } else {
            let shutdown = shutdown.take();
            ready(shutdown.task.notify_starling_errored(shutdown.error))
        }
    }

    fn poll_notify_parent_errored<'a>(
        notify: &'a mut RentToOwn<'a, NotifyParentErrored>
    ) -> Poll<AfterNotifyParentErrored, Void> {
        try_ready!(notify.notification.by_ref().ignore_results::<Void>().poll());
        let notify = notify.take();
        ready(notify.task.notify_starling_errored(notify.error))
    }

    fn poll_notify_starling_errored<'a>(
        notify: &'a mut RentToOwn<'a, NotifyStarlingErrored>
    ) -> Poll<AfterNotifyStarlingErrored, Void> {
        try_ready!(notify.notification.by_ref().ignore_results::<Void>().poll());
        ready(Finished(()))
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
///
/// This needs to be `pub` because it is used in a trait implementation; don't
/// actually use it!
#[derive(Clone)]
#[doc(hidden)]
pub struct TaskHandle {
    id: TaskId,
    sender: mpsc::Sender<TaskMessage>,
}

impl TaskHandle {
    /// Get this task's ID.
    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    /// Send a message to the task.
    pub(crate) fn send(&self, msg: TaskMessage) -> futures::sink::Send<mpsc::Sender<TaskMessage>> {
        self.sender.clone().send(msg)
    }
}

impl fmt::Debug for TaskHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TaskHandle {{ {:?} }}", self.id)
    }
}

/// Messages that can be sent to a task.
///
/// This needs to be `pub` because it is used in a trait implementation; don't
/// actually use it!
#[derive(Debug)]
#[doc(hidden)]
pub enum TaskMessage {
    /// A shutdown request sent from a parent task to its child.
    Shutdown,

    /// A notification that a child task finished OK. Sent from a child task to
    /// its parent.
    ChildTaskFinished {
        /// The ID of the child task that finished OK.
        child: TaskId,
    },

    /// A notification that a child task failed. Sent from the failed child task
    /// to its parent.
    ChildTaskErrored {
        /// The ID of the child task that failed.
        child: TaskId,
        /// The error that the child task failed with.
        error: Error,
    },

    /// A notification of an unhandled rejected promise. Sent from a future in
    /// this task's thread to this task.
    UnhandledRejectedPromise {
        /// The rejection error value that was not handled.
        error: Error,
    },
}
