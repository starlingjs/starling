async function main() {
  let awaited = false;
  let errored = false;

  try {
    await spawn("xxx-some-file-that-really-does-not-exist.js");
    awaited = true;
  } catch (_) {
    errored = true;
  }

  if (awaited) {
    print("should not have awaited");
    throw new Error;
  }

  if (!errored) {
    print("should have errored");
    throw new Error;
  }
}
