//# starling-not-a-test

async function main() {
  print("10.js: spawned");
  await spawn("./9.js");
  print("10.js: done");
}
