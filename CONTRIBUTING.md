# Contributing to Starling

Hi! We'd love to have your contributions! If you want help or mentorship, reach
out to us in a GitHub issue, or ping `fitzgen` in
[#servo on irc.mozilla.org](irc://irc.mozilla.org#servo) and introduce yourself.

<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->


- [Code of Conduct](#code-of-conduct)
- [Building](#building)
- [Testing](#testing)
- [Pull Requests and Code Review](#pull-requests-and-code-review)
- [Automatic Code Formatting](#automatic-code-formatting)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## Code of Conduct

We abide by the [Rust Code of Conduct][coc] and ask that you do as well.

[coc]: https://www.rust-lang.org/en-US/conduct.html

## Building

To build Starling, you need `autoconf` 2.13 and `libclang` at least 3.9. You
should set the `LIBCLANG_PATH` environment variable to the directory containing
`libclang.so`.

For example, in Ubuntu 16.04:

```
$ sudo apt-get install autoconf2.13 clang-4.0
$ sudo update-alternatives --install /usr/bin/clang clang /usr/bin/clang-4.0 100
$ sudo update-alternatives --install /usr/bin/clang++ clang++ /usr/bin/clang++-4.0 100
$ LIBCLANG_PATH=/usr/lib/llvm-4.0/lib cargo build
```

The `LIBCLANG_PATH` environment variable is only needed by codegen when
compiling `mozjs`.  Once it is built, you can use the usual cargo commands such
as `cargo +nightly test`.

## Testing

To run all the tests:

```
$ cargo test
```

## Pull Requests and Code Review

Ensure that each commit stands alone, and passes tests. This enables better `git
bisect`ing when needed. If your commits do not stand on their own, then rebase
them on top of the latest master and squash them into a single commit.

All pull requests undergo code review before merging. To request review, comment
`r? @github_username_of_reviewer`. They we will respond with `r+` to approve the
pull request, or may leave feedback and request changes to the pull request. Any
changes should be squashed into the original commit.

Unsure who to ask for review? Ask any of:

* `@fitzgen`
* `@tschneidereit`

More resources:

* [Servo's GitHub Workflow](https://github.com/servo/servo/wiki/Github-workflow)
* [Beginner's Guide to Rebasing and Squashing](https://github.com/servo/servo/wiki/Beginner's-guide-to-rebasing-and-squashing)

## Automatic Code Formatting

We use [`rustfmt`](https://github.com/rust-lang-nursery/rustfmt) to enforce a
consistent code style across the whole code base.

You can install the latest version of `rustfmt` with this command:

```
$ rustup update nightly
$ cargo +nightly install -f rustfmt-nightly
```

Ensure that `~/.cargo/bin` is on your path.

Once that is taken care of, you can (re)format all code by running this command:

```
$ cargo +nightly fmt
```
