//# starling-fail
//# starling-stdout-has: parent: throwing error in `main`
//# starling-stdout-has: child: uh uh uh uh, staying alive, staying alive
//# starling-stderr-has: error: tests/js/child-outlives-parent-error.js:15:9:

async function main() {
  // Spawn it, but don't await it so that we can ensure that the child task gets
  // reclaimed once we exit.
  spawn("./stay-alive-forever.js");

  // This should be enough time for the child to spawn and print to stdout.
  await timeout(500);

  print("parent: throwing error in `main`");
  throw new Error;
}
