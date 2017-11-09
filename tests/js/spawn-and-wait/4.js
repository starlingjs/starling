//# starling-not-a-test

async function main() {
  print("4.js: spawned");
  await spawn("./3.js");
  print("4.js: done");
}
