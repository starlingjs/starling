//# starling-not-a-test

async function main() {
  print("3.js: spawned");
  await spawn("./2.js");
  print("3.js: done");
}
