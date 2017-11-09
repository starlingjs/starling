//! Definitions related to a task's JavaScript global object.

use futures::Future;
use js;
use js::jsapi;
use js::rust::Runtime as JsRuntime;
use promise_future_glue::future_to_promise;
use std::ffi;
use std::os::raw;
use std::path;
use std::ptr;
use std::time::Duration;
use task;
use tokio_timer::Timer;

js_native_no_panic! {
    /// Print the given arguments to stdout for debugging.
    pub fn print(
        cx: *mut jsapi::JSContext,
        argc: raw::c_uint,
        vp: *mut jsapi::JS::Value
    ) -> bool {
        let args = unsafe {
            jsapi::JS::CallArgs::from_vp(vp, argc)
        };

        for i in 0..argc {
            rooted!(in(cx) let s = unsafe {
                js::rust::ToString(cx, args.index(i))
            });
            if s.get().is_null() {
                return false;
            }

            let char_ptr = unsafe {
                jsapi::JS_EncodeStringToUTF8(cx, s.handle())
            };
            if char_ptr.is_null() {
                return false;
            }

            {
                let cstr = unsafe {
                    ffi::CStr::from_ptr(char_ptr)
                };
                let string = cstr.to_string_lossy();
                print!("{}", string);
            }

            unsafe {
                jsapi::JS_free(cx, char_ptr as *mut _);
            }
        }

        println!();
        true
    }
}

js_native! {
    fn timeout(
        millis: js::conversions::EnforceRange<u64>
    ) -> *mut js::jsapi::JSObject {
        let timer = Timer::default();
        let duration = Duration::from_millis(millis.0);
        let future = timer.sleep(duration).map_err(|e| e.to_string());
        let promise = future_to_promise(future);
        unsafe {
            promise.raw()
        }
    }
}

js_native! {
    fn spawn(
        js_file: String
    ) -> *mut js::jsapi::JSObject {
        let mut file = task::this_js_file();
        file.pop();
        file.push(path::Path::new(&js_file));

        let starling = task::starling_handle();
        match task::Task::spawn_child(starling, file) {
            Ok(child) => {
                unsafe {
                    child.raw()
                }
            }
            Err(_) => {
                unsafe {
                    js::glue::ReportError(
                        JsRuntime::get(),
                        b"failed to spawn new task\0" as *const u8 as *const _
                    );
                }
                ptr::null_mut()
            }
        }
    }
}

lazy_static! {
    pub static ref GLOBAL_FUNCTIONS: [jsapi::JSFunctionSpec; 4] = [
        jsapi::JSFunctionSpec::js_fn(
            b"print\0".as_ptr() as *const _,
            Some(print),
            0,
            0
        ),
        jsapi::JSFunctionSpec::js_fn(
            b"timeout\0".as_ptr() as *const _,
            Some(timeout::js_native),
            1,
            0
        ),
        jsapi::JSFunctionSpec::js_fn(
            b"spawn\0".as_ptr() as *const _,
            Some(spawn::js_native),
            1,
            0
        ),
        jsapi::JSFunctionSpec::NULL
    ];
}
