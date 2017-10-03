//! Infrastructure for tracking unhandled rejected promises.

use js;
use js::heap::Trace;
use js::jsapi;
use js::rust::Runtime as JsRuntime;
use std::cell::RefCell;
use std::mem;
use std::os;

/// Keeps track of unhandled, rejected promises.
#[derive(Debug, Default)]
pub struct RejectedPromisesTracker {
    unhandled_rejected_promises: Vec<Box<js::heap::Heap<*mut jsapi::JSObject>>>,
}

impl RejectedPromisesTracker {
    /// Register a promises tracker with the given runtime.
    pub fn register(runtime: &JsRuntime, tracker: &RefCell<Self>) {
        unsafe {
            let tracker: *const RefCell<Self> = tracker as _;
            let tracker = tracker as *mut os::raw::c_void;
            jsapi::JS::SetPromiseRejectionTrackerCallback(
                runtime.cx(),
                Some(Self::promise_rejection_callback),
                tracker,
            );
        }
    }

    /// Clear the set of unhandled, rejected promises.
    pub fn clear(&mut self) {
        self.unhandled_rejected_promises.clear();
    }

    /// Take this tracker's set of unhandled, rejected promises, leaving an
    /// empty set in its place.
    pub fn take(&mut self) -> Vec<Box<js::heap::Heap<*mut jsapi::JSObject>>> {
        mem::replace(&mut self.unhandled_rejected_promises, vec![])
    }

    /// The given rejected `promise` has been handled, so remove it from the
    /// unhandled set.
    fn handled(&mut self, promise: jsapi::JS::HandleObject) {
        let idx = self.unhandled_rejected_promises
            .iter()
            .position(|p| p.get() == promise.get());

        if let Some(idx) = idx {
            self.unhandled_rejected_promises.swap_remove(idx);
        }
    }

    /// The given rejected `promise` has not been handled, so add it to the
    /// rejected set if necessary.
    fn unhandled(&mut self, promise: jsapi::JS::HandleObject) {
        let idx = self.unhandled_rejected_promises
            .iter()
            .position(|p| p.get() == promise.get());

        if let None = idx {
            // This needs to be performed in two steps because Heap's
            // post-write barrier relies on the Heap instance having a
            // stable address.
            self.unhandled_rejected_promises
                .push(Box::new(js::heap::Heap::default()));
            self.unhandled_rejected_promises
                .last()
                .unwrap()
                .set(promise.get());
        }
    }

    /// Raw JSAPI callback for observing rejected promise handling.
    unsafe extern "C" fn promise_rejection_callback(
        _cx: *mut jsapi::JSContext,
        promise: jsapi::JS::HandleObject,
        state: jsapi::PromiseRejectionHandlingState,
        data: *mut os::raw::c_void,
    ) {
        let tracker: *const RefCell<Self> = data as _;
        let tracker: &RefCell<Self> = tracker.as_ref().unwrap();
        let mut tracker = tracker.borrow_mut();

        if state == jsapi::PromiseRejectionHandlingState::Handled {
            tracker.handled(promise);
        } else {
            assert_eq!(state, jsapi::PromiseRejectionHandlingState::Unhandled);
            tracker.unhandled(promise);
        }
    }
}

unsafe impl Trace for RejectedPromisesTracker {
    unsafe fn trace(&self, tracer: *mut jsapi::JSTracer) {
        for promise in &self.unhandled_rejected_promises {
            promise.trace(tracer);
        }
    }
}
