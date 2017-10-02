//! Definitions related to a task's JavaScript global object.

use js::{self, jsapi};
use std::ffi;
use std::os::raw;

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

lazy_static! {
    pub static ref GLOBAL_FUNCTIONS: [jsapi::JSFunctionSpec; 2] = [
        jsapi::JSFunctionSpec::js_fn(
            b"print\0".as_ptr() as *const _,
            Some(print),
            0,
            0
        ),
        jsapi::JSFunctionSpec::NULL
    ];
}
