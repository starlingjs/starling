//# starling-stdout-has: done

async function main() {
  // Spawn a child task but do not await on it. It should get killed after this
  // parent task exits, and then reclaimed by the Starling system supervisor
  // thread.
  spawn("./stay-alive-forever.js")
  await timeout(0);
  print("done");
}
