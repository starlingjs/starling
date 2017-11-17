async function main() {
  let errored = false;
  try {
    await spawn("./simple-exit-1.js");
  } catch (_) {
    errored = true;
  }

  if (!errored) {
    print("did not throw an error, when we expected one");
    throw new Error;
  }
}
