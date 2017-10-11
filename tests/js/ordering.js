//# starling-stdout-has: done

// The top level is evaluated, then the micro-task queue is drained, then `main`
// is evaluated and the micro-task queue is drained again. Once `main`
// completes, we drain the micro-task queue once more and then the task
// completes.

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

assertEvent(0);

(async function () {
  assertEvent(1);
  await Promise.resolve();
  assertEvent(3);
}());

assertEvent(2);

async function main() {
  assertEvent(4);

  // Micro-task queue always happens before any new ticks of the event loop.
  await Promise.all([
    Promise.resolve().then(_ => {
      assertEvent(5);
    }),
    timeout(0).then(_ => {
      assertEvent(6);
    }),
  ]);

  assertEvent(7);

  // No await: we're testing that the micro-task queue is flushed one last time
  // after `main` exits.
  Promise.resolve().then(_ => {
    assertEvent(8);
    print("done");
  });
}
