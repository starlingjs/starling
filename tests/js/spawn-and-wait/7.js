//# starling-not-a-test

async function main() {
  print("7.js: spawned");
  await spawn("./6.js");
  print("7.js: done");
}
