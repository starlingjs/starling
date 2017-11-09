//# starling-not-a-test

async function main() {
  print("9.js: spawned");
  await spawn("./8.js");
  print("9.js: done");
}
