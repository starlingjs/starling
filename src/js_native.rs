//! Helpers for exposing native functions to JavaScript.

/// Wrap a `JSNative` function with a catch unwind to ensure we don't panic and
/// unwind across language boundaries, which is undefined behavior.
#[macro_export]
macro_rules! js_native_no_panic {
    (
        $( #[$attr:meta] )*
        pub fn $name:ident ( $( $arg_name:ident : $arg_ty:ty $(,)* )* ) -> bool {
            $( $body:tt )*
        }
    ) => {
        $( #[$attr] )*
        pub extern "C" fn $name( $( $arg_name : $arg_ty , )* ) -> bool {
            match ::std::panic::catch_unwind(move || {
                $( $body )*
            }) {
                Ok(b) => b,
                Err(_) => {
                    eprintln!(
                        concat!(
                            "error: JSNative '",
                            stringify!($name),
                            "' panicked: must not panic across FFI boundaries!"
                        )
                    );
                    false
                }
            }
        }
    }
}

macro_rules! len_idents {
    () => {
        0
    };
    ( $x:ident $( $xs:ident )* ) => {
        1 + len_idents!( $( $xs )* )
    }
}

/// Define a native Rust function that can be exposed to JS as a `JSNative`.
///
/// All parameter types must implement the
/// `js::conversions::FromJSValConvertible<Config = ()>` trait.
///
/// The return type must implement `js::conversions::ToJSValConvertible`.
///
/// The resulting `JSNative` function will be `$name::js_native`, and will
/// automatically perform conversion of parameter and return types.
#[macro_export]
macro_rules! js_native {
    (
        $( #[$attr:meta] )*
        fn $name:ident ( $( $arg_name:ident : $arg_ty:ty $(,)* )* ) -> $ret_ty:ty {
            $( $body:tt )*
        }
    ) => {
        fn $name ( $( $arg_name : $arg_ty , )* ) -> $ret_ty {
            $( $body )*
        }

        mod $name {
            use js;
            use std::os::raw;

            js_native_no_panic! {
                pub fn js_native(
                    cx: *mut js::jsapi::JSContext,
                    argc: raw::c_uint,
                    vp: *mut js::jsapi::JS::Value
                ) -> bool {

                    // Check that the required number of arguments are passed in.

                    let num_required_args = len_idents!( $( $arg_name )* );
                    if argc < num_required_args {
                        unsafe {
                            js::glue::ReportError(
                                cx,
                                concat!(
                                    "Not enough arguments to `",
                                    stringify!($name),
                                    "`"
                                ).as_ptr() as *const _
                            );
                        }
                        return false;
                    }

                    // Convert each argument into the expected Rust type.

                    let args = unsafe {
                        js::jsapi::JS::CallArgs::from_vp(vp, argc)
                    };

                    let mut i = 0;
                    $(
                        let $arg_name = args.index({
                            assert!(i < argc);
                            let j = i;
                            i += 1;
                            j
                        });
                        let $arg_name = match unsafe {
                            <$arg_ty as js::conversions::FromJSValConvertible>::from_jsval(
                                cx,
                                $arg_name,
                                ()
                            )
                        } {
                            Err(()) => {
                                debug_assert!(unsafe {
                                    js::jsapi::JS_IsExceptionPending(cx)
                                });
                                return false;
                            }
                            Ok(js::conversions::ConversionResult::Failure(e)) => {
                                unsafe {
                                    js::glue::ReportError(cx, e.as_ptr() as _);
                                }
                                return false;
                            }
                            Ok(js::conversions::ConversionResult::Success(x)) => x,
                        };
                    )*

                    // Call the function and then convert the return type into a
                    // JS value.

                    let ret = super::$name( $( $arg_name , )* );
                    unsafe {
                        <$ret_ty as js::conversions::ToJSValConvertible>::to_jsval(
                            &ret,
                            cx,
                            args.rval()
                        );
                    }

                    true
                }
            }
        }
    }
}
