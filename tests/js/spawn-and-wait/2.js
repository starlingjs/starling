//# starling-not-a-test

async function main() {
  print("2.js: spawned");
  await spawn("./1.js");
  print("2.js: done");
}
