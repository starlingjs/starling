// The `timeout` function defined via `js_native!` should exist.

if (typeof timeout != "function") {
  throw new Error("typeof timeout != function");
}
