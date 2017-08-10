#!/usr/bin/env bash

set -eux

# We seem to be OOMing with more build jobs.
export CARGO_BUILD_JOBS=6

cargo build $PROFILE $FEATURES
cargo test $PROFILE $FEATURES

if [[ "$PROFILE" == "--release" && "$FEATURES" == "" ]]; then
    cargo bench
fi
