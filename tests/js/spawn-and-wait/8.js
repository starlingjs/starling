//# starling-not-a-test

async function main() {
  print("8.js: spawned");
  await spawn("./7.js");
  print("8.js: done");
}
