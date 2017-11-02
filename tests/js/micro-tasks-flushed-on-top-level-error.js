//# starling-fail
//# starling-stdout-has: micro

const assertEvent = (function () {
  let currentEvent = 0;
  return expected => {
    print("assertEvent(", expected, ") @ ", currentEvent);
    if (currentEvent != expected) {
      throw new Error("expected event " + expected + " but saw event " + currentEvent);
    }
    currentEvent++;
  };
}());

Promise.resolve().then(_ => {
  assertEvent(1);
  print("micro")
});

assertEvent(0);
throw new Error();
