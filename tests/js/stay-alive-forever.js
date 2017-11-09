//# starling-not-a-test

async function main() {
  // Loop forever, but yield to the event loop so that we can get killed
  // properly if the parent finishes.
  while (true) {
    print("child: uh uh uh uh, staying alive, staying alive");
    await timeout(999999999999);
  }
}
