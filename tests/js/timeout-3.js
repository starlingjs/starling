//# starling-stdout-has: after await timeout

async function main() {
  print("before await timeout");
  await timeout(0);
  print("after await timeout");
}
