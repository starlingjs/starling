//# starling-not-a-test

async function main() {
  print("1.js: spawned");
  await spawn("./0.js");
  print("1.js: done");
}
