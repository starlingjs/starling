//# starling-fail

function main() {
  // Starling will wait on this promise, but then it gets GC'd which results in
  // an error.
  return new Promise();
}
