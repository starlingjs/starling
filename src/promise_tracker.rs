//! Infrastructure for tracking unhandled rejected promises.

use super::UnhandledRejectedPromises;
use gc_roots::GcRoot;
use js::jsapi;
use js::rust::Runtime as JsRuntime;
use std::cell::RefCell;
use std::os;

/// Keeps track of unhandled, rejected promises.
#[derive(Debug, Default)]
pub struct RejectedPromisesTracker {
    unhandled_rejected_promises: Vec<GcRoot<*mut jsapi::JSObject>>,
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
    pub fn take(&mut self) -> Option<UnhandledRejectedPromises> {
        if self.unhandled_rejected_promises.is_empty() {
            return None;
        }

        let cx = JsRuntime::get();
        let rejected = unsafe {
            let unhandled = self.unhandled_rejected_promises.drain(..);
            UnhandledRejectedPromises::from_promises(cx, unhandled)
        };
        debug_assert!(self.unhandled_rejected_promises.is_empty());
        Some(rejected)
    }

    /// The given rejected `promise` has been handled, so remove it from the
    /// unhandled set.
    fn handled(&mut self, promise: jsapi::JS::HandleObject) {
        let idx = self.unhandled_rejected_promises
            .iter()
            .position(|p| unsafe { p.raw() == promise.get() });

        if let Some(idx) = idx {
            self.unhandled_rejected_promises.swap_remove(idx);
        }
    }

    /// The given rejected `promise` has not been handled, so add it to the
    /// rejected set if necessary.
    fn unhandled(&mut self, promise: jsapi::JS::HandleObject) {
        let idx = self.unhandled_rejected_promises
            .iter()
            .position(|p| unsafe { p.raw() == promise.get() });

        if let None = idx {
            self.unhandled_rejected_promises
                .push(GcRoot::new(promise.get()));
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
