# `starling`

[![Build Status](https://travis-ci.org/starlingjs/starling.png?branch=master)](https://travis-ci.org/starlingjs/starling)

The Starling JavaScript runtime.

## Building

To build starling, you need `autoconf` 2.13 and `clang` at least 3.9. You should set the `LIBCLANG_PATH` environment
variable to the directory containing `libclang.so.1`.

For example, in Ubuntu 16.04:
```
sudo apt-get install autoconf2.13 clang-4.0
LIBCLANG_PATH=/usr/lib/llvm-4.0/lib/ cargo +nightly build
```

The `LIBCLANG_PATH` environment variable is only needed by codegen when compiling `mozjs`.
Once it is built, you can use the usual cargo commands such as `cargo +nightly test`.
