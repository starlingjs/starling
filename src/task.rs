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

use super::{Error, ErrorKind, Result, StarlingHandle, StarlingMessage};
use futures::{self, Async, Future, Sink};
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
use promise_future_glue::{promise_to_future, Promise2Future};
use promise_tracker::RejectedPromisesTracker;
use std::cell::RefCell;
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
    static EVENT_LOOP: RefCell<Option<tokio_core::reactor::Handle>> = RefCell::new(None);
}

/// Get a handle to this thread's `tokio` event loop.
pub fn event_loop() -> tokio_core::reactor::Handle {
    EVENT_LOOP.with(|el| {
        el.borrow()
            .clone()
            .expect("called `task::event_loop` before initializing thread's event loop")
    })
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
    rejected_promises: RefCell<RejectedPromisesTracker>,
}

impl Drop for Task {
    fn drop(&mut self) {
        unsafe {
            jsapi::JS_LeaveCompartment(self.runtime().cx(), ptr::null_mut());
        }

        self.global.set(ptr::null_mut());
        self.rejected_promises.borrow_mut().clear();
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

    fn create_main(starling: StarlingHandle, js_file: path::PathBuf) -> Result<Box<Task>> {
        let runtime = Some(JsRuntime::new(true).map_err(|_| {
            Error::from_kind(ErrorKind::CouldNotCreateJavaScriptRuntime)
        })?);

        let (sender, receiver) =
            mpsc::channel(starling.options().buffer_capacity_for::<TaskMessage>());

        let task = Box::new(Task {
            handle: TaskHandle {
                id: TaskId(thread::current().id()),
                sender,
            },
            receiver,
            global: js::heap::Heap::default(),
            runtime,
            starling,
            js_file,
            parent: None,
            state: State::Created,
            rejected_promises: RefCell::new(RejectedPromisesTracker::default()),
        });

        GcRootSet::initialize();
        RejectedPromisesTracker::register(task.runtime(), &task.rejected_promises);
        task.create_global();

        Ok(task)
    }

    /// Create a new child task.
    ///
    /// The child task's lifetime is constrained within its parent task's
    /// lifetime. When the parent task terminates, the child is terminated as
    /// well.
    pub fn create_child(starling: StarlingHandle, parent: TaskHandle) -> Result<TaskHandle> {
        let _ = starling;
        let _ = parent;
        unimplemented!()
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

        let rejected_promises = self.rejected_promises.borrow();
        rejected_promises.trace(tracer);

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

    // Check for unhandled, rejected promises and return an error if any exist.
    fn check_for_unhandled_rejected_promises(&mut self) -> Result<()> {
        let unhandled = {
            let mut tracker = self.rejected_promises.borrow_mut();
            tracker.take()
        };
        if !unhandled.is_empty() {
            // TODO: collect the rejection value from the promise, and
            // line/column/etc if the rejection value is some kind of Error
            // object.
            let err = ErrorKind::JavaScriptUnhandledRejectedPromise;
            return Err(err.into());
        }
        Ok(())
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
                // TODO: convert the pending exception into a meaningful error.
                jsapi::JS_ClearPendingException(cx);

                jsapi::js::RunJobs(cx);
            }
            return self.notify_starling_errored(Error::from_kind(ErrorKind::JavaScriptException));
        }

        unsafe {
            jsapi::js::RunJobs(cx);
        }

        if let Err(e) = self.check_for_unhandled_rejected_promises() {
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
                if jsapi::JS_IsExceptionPending(cx) {
                    // TODO: convert the pending exception into a meaningful error.
                    jsapi::JS_ClearPendingException(cx);
                }

                // TODO: It isn't obvious that we should drain the micro-task
                // queue here. But it also isn't obvious that we shouldn't.
                // Let's investigate this sometime in the future.
                jsapi::js::RunJobs(cx);
                return self.notify_starling_errored(
                    Error::from_kind(ErrorKind::JavaScriptException)
                );
            }

            jsapi::js::RunJobs(cx);
        }

        if let Err(e) = self.check_for_unhandled_rejected_promises() {
            return self.notify_starling_errored(e);
        }

        rooted!(in(cx) let mut obj = ptr::null_mut());
        if unsafe {
            rval.get().is_object() && {
                obj.set(rval.get().to_object());
                jsapi::JS::IsPromiseObject(obj.handle())
            }
        } {
            let obj = GcRoot::new(obj.get());
            let future = promise_to_future(&obj);
            self.state = State::WaitingOnPromise(future);
            self.poll()
        } else {
            self.notify_starling_finished()
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
        if let Some(parent) = self.parent.as_ref() {
            let notify = parent.send(TaskMessage::ChildTaskFinished(self.id()));
            self.state = State::NotifyParentFinished(notify);
        } else {
            // Don't have a parent to notify, so we're all done!
            return Ok(Async::Ready(()));
        }
        self.poll()
    }

    fn notify_parent_errored(&mut self, error: Error) -> Result<Async<()>> {
        assert!(self.state.is_notify_starling_errored());
        if let Some(parent) = self.parent.as_ref() {
            let notify = parent.send(TaskMessage::ChildTaskErrored(self.id(), error));
            self.state = State::NotifyParentErrored(notify);
        } else {
            // Don't have a parent to notify, so we're all done!
            return Ok(Async::Ready(()));
        }
        self.poll()
    }
}

#[derive(is_enum_variant)]
enum State {
    Created,
    ReadingJsModule(CpuFuture<String, Error>),
    WaitingOnPromise(Promise2Future<jsapi::JS::HandleValue, jsapi::JS::HandleValue>),

    NotifyStarlingFinished(futures::sink::Send<mpsc::Sender<StarlingMessage>>),
    NotifyParentFinished(futures::sink::Send<mpsc::Sender<TaskMessage>>),

    NotifyStarlingErrored(Error, futures::sink::Send<mpsc::Sender<StarlingMessage>>),
    NotifyParentErrored(futures::sink::Send<mpsc::Sender<TaskMessage>>),
}

enum NextState {
    EvaluateTopLevel(String),
    NotifyStarlingFinished,
    NotifyStarlingErrored(Error),
    NotifyParentFinished,
    NotifyParentErrored(Error),
}

impl Future for Task {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>> {
        let next_state = match self.state {
            State::Created => {
                return self.read_js_module();
            }
            State::ReadingJsModule(ref mut reading) => {
                let src = try_ready!(reading.poll());
                NextState::EvaluateTopLevel(src)
            }
            State::WaitingOnPromise(ref mut promise) => {
                match try_ready!(promise.poll()) {
                    Err(_rejection) => {
                        let err = ErrorKind::JavaScriptUnhandledRejectedPromise;
                        let err = err.into();
                        NextState::NotifyStarlingErrored(err)
                    }
                    Ok(_resolution) => {
                        NextState::NotifyStarlingFinished
                    }
                }
            }
            State::NotifyStarlingFinished(ref mut notify) => {
                try_ready!(
                    notify
                        .poll()
                        .map_err(|_| "couldn't notify starling of task finished")
                );
                NextState::NotifyParentFinished
            }
            State::NotifyParentFinished(ref mut notify) => {
                try_ready!(
                    notify
                        .poll()
                        .map_err(|_| "couldn't notify parent of task finished")
                );
                return Ok(Async::Ready(()));
            }
            State::NotifyStarlingErrored(ref error, ref mut notify) => {
                try_ready!(
                    notify
                        .poll()
                        .map_err(|_| "couldn't notify starling of task errored")
                );
                NextState::NotifyParentErrored(error.clone())
            }
            State::NotifyParentErrored(ref mut notify) => {
                try_ready!(
                    notify
                        .poll()
                        .map_err(|_| "couldn't notify parent of task errored")
                );
                return Ok(Async::Ready(()));
            }
        };

        match next_state {
            NextState::EvaluateTopLevel(src) => self.evaluate_top_level(src),
            NextState::NotifyStarlingFinished => self.notify_starling_finished(),
            NextState::NotifyStarlingErrored(e) => self.notify_starling_errored(e),
            NextState::NotifyParentFinished => self.notify_parent_finished(),
            NextState::NotifyParentErrored(e) => self.notify_parent_errored(e),
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
}
