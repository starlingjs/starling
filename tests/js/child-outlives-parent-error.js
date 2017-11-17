//# starling-fail

async function main() {
  // Spawn it, but don't await it so that we can ensure that the child task gets
  // reclaimed once we exit.
  spawn("./stay-alive-forever.js");

  print("parent: throwing error in `main`");
  throw new Error;
}
