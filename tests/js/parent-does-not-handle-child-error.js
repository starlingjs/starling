//# starling-fail
//# starling-stderr-has: error: 1 unhandled rejected promise(s)
//# starling-stderr-has: this should cause an exit 1

async function main() {
  // Spawn a child task that will error. Since we don't handle the child error,
  // it will propagate as an unhandled, rejected promise and kill this task as
  // well.
  print("parent: spawning simple-exit-1.js");
  spawn("./simple-exit-1.js");

  // Wait for the error to propagate.
  while (true) {
    await timeout(9999);
  }
}
