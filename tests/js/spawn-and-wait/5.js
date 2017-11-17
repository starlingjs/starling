//# starling-not-a-test

async function main() {
  print("5.js: spawned");
  await spawn("./4.js");
  print("5.js: done");
}
