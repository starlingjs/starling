//# starling-not-a-test

async function main() {
  print("6.js: spawned");
  await spawn("./5.js");
  print("6.js: done");
}
