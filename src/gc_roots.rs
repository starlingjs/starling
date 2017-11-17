//! Keeping GC-things alive and preventing them from being collected.

use js::conversions::{ConversionResult, FromJSValConvertible, ToJSValConvertible};
use js::heap::{Heap, Trace};
use js::jsapi;
use js::rust::GCMethods;
use std::cell::{Ref, RefCell};
use std::rc::{Rc, Weak};

thread_local! {
    static TASK_GC_ROOTS: RefCell<Option<GcRootSet>> = RefCell::new(None);
}

/// The set of persistent GC roots for a task.
///
/// When we create `GcRoot`s, we register them here. This set is unconditionally
/// marked during GC root marking for a JS task, so anything registered within
/// will be kept alive and won't be reclaimed by the collector.
///
/// The actual GC thing being rooted lives within the `GcRootInner`, which is
/// reference counted. Whenever we trace this root set, we clear out any entries
/// whose reference count has dropped to zero.
#[derive(Default)]
pub(crate) struct GcRootSet {
    objects: RefCell<Vec<Weak<GcRootInner<*mut jsapi::JSObject>>>>,
    values: RefCell<Vec<Weak<GcRootInner<jsapi::JS::Value>>>>,
}

unsafe impl Trace for GcRootSet {
    unsafe fn trace(&self, tracer: *mut jsapi::JSTracer) {
        let mut objects = self.objects.borrow_mut();
        objects.retain(|entry| {
            if let Some(inner) = entry.upgrade() {
                let inner = inner.ptr.borrow();
                let inner = inner
                    .as_ref()
                    .expect("should not have severed while GcRootSet is initialized");
                inner.trace(tracer);
                true
            } else {
                // Don't keep this root -- no one is using it anymore.
                false
            }
        });

        let mut values = self.values.borrow_mut();
        values.retain(|entry| {
            if let Some(inner) = entry.upgrade() {
                let inner = inner.ptr.borrow();
                let inner = inner
                    .as_ref()
                    .expect("should not have severed while GcRootSet is initialized");
                inner.trace(tracer);
                true
            } else {
                false
            }
        });
    }
}

impl GcRootSet {
    /// Gain immutable access to the current task's set of `Future2Promise` GC
    /// roots.
    ///
    /// This is _not_ reentrant! Do not call into JS or do anything that might
    /// GC within this function.
    #[inline]
    pub fn with_ref<F, T>(mut f: F) -> T
    where
        F: FnMut(&Self) -> T,
    {
        TASK_GC_ROOTS.with(|roots| {
            let roots = roots.borrow();
            let roots = roots.as_ref().expect(
                "Should only call `GcRootSet::with_ref` after \
                 `GcRootSet::initialize`",
            );
            f(&*roots)
        })
    }

    /// Gain mutable access to the current task's set of `Future2Promise` GC
    /// roots.
    ///
    /// This is _not_ reentrant! Do not call into JS or do anything that might
    /// GC within this function.
    #[inline]
    pub fn with_mut<F, T>(mut f: F) -> T
    where
        F: FnMut(&mut Self) -> T,
    {
        TASK_GC_ROOTS.with(|roots| {
            let mut roots = roots.borrow_mut();
            let roots = roots.as_mut().expect(
                "Should only call `GcRootSet::with_mut` after \
                 `GcRootSet::initialize`",
            );
            f(&mut *roots)
        })
    }

    /// Initialize the current JS task's GC roots set.
    pub fn initialize() {
        TASK_GC_ROOTS.with(|r| {
            let mut r = r.borrow_mut();

            assert!(
                r.is_none(),
                "should never call `GcRootSet::initialize` more than \
                 once per task"
            );

            *r = Some(Self::default());
        });
    }

    /// Destroy the current JS task's GC roots set.
    pub fn uninitialize() {
        TASK_GC_ROOTS.with(|r| {
            let mut r = r.borrow_mut();

            {
                let roots = r.as_ref().expect(
                    "should only call `GcRootSet::uninitialize` on initialized `GcRootSet`s",
                );

                // You might be expecting an assertion that all of the
                // registered roots have a reference count of zero here. You
                // won't find it.
                //
                // We allow for registered roots that still have greater than
                // zero reference counts during shutdown. Their existence is
                // safe, but dereferencing them is most definitely not! We're
                // pushing dangerously close to UAF here, and the invariant is
                // somewhat subtle.
                //
                // So what do we gain by relaxing "no greater than zero
                // reference counts on roots at shutdown" into "no dereferencing
                // roots after shutdown"? It allows us to embed `GcRoot`
                // directly into futures that we spawn into the `tokio` event
                // loop. Spawning a future gives up ownership of it, and we
                // can't ensure that its lifetime is contained within the JS
                // task's lifetime. This makes the stronger invariant impossible
                // to maintain. Instead, we add the ability "sever" outstanding
                // GC roots by setting the `GcRootInner::ptr` to `None` and
                // dropping the wrapped `js::heap::Heap`. Note that we have to
                // make sure all `js::heap::Heap`s *do* maintain the stronger
                // invariant, because their write barriers are fired upon
                // destruction, which would lead to UAF if we didn't maintain
                // the stronger invariant for them.

                let objects = roots.objects.borrow();
                for entry in &*objects {
                    if let Some(entry) = entry.upgrade() {
                        entry.sever();
                    }
                }

                let values = roots.values.borrow();
                for entry in &*values {
                    if let Some(entry) = entry.upgrade() {
                        entry.sever();
                    }
                }
            }

            *r = None;
        });
    }
}

/// A trait implemented by types that can be rooted by Starling.
///
/// This should *not* be implemented by anything outside of the Starling crate!
/// It is only exposed because it is a trait bound on other `pub` types.
pub unsafe trait GcRootable: GCMethods + Copy {
    #[doc(hidden)]
    unsafe fn root(&GcRoot<Self>);
}

unsafe impl GcRootable for *mut jsapi::JSObject {
    #[inline]
    unsafe fn root(root: &GcRoot<*mut jsapi::JSObject>) {
        GcRootSet::with_mut(|roots| {
            let mut objects = roots.objects.borrow_mut();
            objects.push(Rc::downgrade(&root.inner));
        });
    }
}

unsafe impl GcRootable for jsapi::JS::Value {
    #[inline]
    unsafe fn root(root: &GcRoot<jsapi::JS::Value>) {
        GcRootSet::with_mut(|roots| {
            let mut values = roots.values.borrow_mut();
            values.push(Rc::downgrade(&root.inner));
        });
    }
}

#[derive(Debug)]
struct GcRootInner<T: GcRootable> {
    ptr: RefCell<Option<Heap<T>>>,
}

impl<T> Default for GcRootInner<T>
where
    T: GcRootable,
    Heap<T>: Default,
{
    fn default() -> Self {
        GcRootInner {
            ptr: RefCell::new(Some(Heap::default())),
        }
    }
}

impl<T> GcRootInner<T>
where
    T: GcRootable,
{
    // See `GcRootSet::uninitialize` for details.
    fn sever(&self) {
        let mut ptr = self.ptr.borrow_mut();
        *ptr = None;
    }
}

/// A persistent GC root.
///
/// These should never be stored within some object that is managed by the GC,
/// only for outside-the-gc-heap --> inside-the-gc-heap edges.
///
/// Unlike the `js` crate's `rooted!(in(cx) ...)` and SpiderMonkey's
/// `JS::Rooted` type, these do not need to maintain a strict LIFO ordering on
/// their construction and destruction, and may persist in the heap.
#[derive(Debug, Clone)]
pub struct GcRoot<T: GcRootable> {
    inner: Rc<GcRootInner<T>>,
}

impl<T> Default for GcRoot<T>
where
    T: GcRootable,
    Heap<T>: Default,
{
    fn default() -> Self {
        GcRoot {
            inner: Rc::new(GcRootInner::default()),
        }
    }
}

impl<T> GcRoot<T>
where
    T: GcRootable,
    Heap<T>: Default,
{
    /// Root the given GC thing, so that it is not reclaimed by the collector.
    #[inline]
    pub fn new(value: T) -> GcRoot<T> {
        let root = GcRoot::default();
        unsafe {
            GcRootable::root(&root);
        }

        root.borrow().set(value);
        root
    }

    /// Borrow the underlying GC thing.
    ///
    /// ## Panics
    ///
    /// Panics if the JS task has already shutdown.
    #[inline]
    pub fn borrow(&self) -> Ref<Heap<T>> {
        Ref::map(self.inner.ptr.borrow(), |r| {
            r.as_ref().expect("should not be severed")
        })
    }

    /// Get a raw pointer the underlying GC thing.
    ///
    /// This is mostly useful for translating from a `GcRoot` into a
    /// `JS::Rooted`:
    ///
    /// ```ignore
    /// // Given we have some `GcRoot`,
    /// let some_gc_root: GcRoot<*mut jsapi::JSObject> = ...;
    ///
    /// // We can convert it into a `JS::Rooted` so we can create `JS::Handle`s
    /// // and `JS::MutableHandle`s.
    /// rooted!(in(cx) let rooted = unsafe { some_gc_root.raw() });
    /// ```
    ///
    /// ## Panics
    ///
    /// Panics if the JS task has already shutdown.
    ///
    /// ## Unsafety
    ///
    /// SpiderMonkey has a moving GC, so there is no guarantee that the returned
    /// pointer will continue to point at the right thing.
    ///
    /// If this `GcRoot` is dropped, there is no guarantee that the pointer will
    /// still be live, and that it won't be reclaimed by the GC.
    pub unsafe fn raw(&self) -> T {
        self.borrow().get()
    }
}

impl<T> FromJSValConvertible for GcRoot<T>
where
    T: FromJSValConvertible + GcRootable,
    Heap<T>: Default,
{
    type Config = T::Config;

    #[inline]
    unsafe fn from_jsval(
        cx: *mut jsapi::JSContext,
        val: jsapi::JS::HandleValue,
        config: Self::Config,
    ) -> Result<ConversionResult<Self>, ()> {
        match T::from_jsval(cx, val, config) {
            Ok(cr) => Ok(cr.map(GcRoot::new)),
            Err(()) => Err(()),
        }
    }
}

impl<T> ToJSValConvertible for GcRoot<T>
where
    T: ToJSValConvertible + GcRootable,
    Heap<T>: Default,
{
    #[inline]
    unsafe fn to_jsval(&self, cx: *mut jsapi::JSContext, rval: jsapi::JS::MutableHandleValue) {
        self.raw().to_jsval(cx, rval);
    }
}
