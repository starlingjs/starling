//# starling-fail
//# starling-stderr-has: error: 6 unhandled rejected promise(s)
//# starling-stderr-has: uno
//# starling-stderr-has: dos
//# starling-stderr-has: tres
//# starling-stderr-has: the froggle is the wrong bodoozer
//# starling-stderr-has: woops
//# starling-stderr-has: how many promises do I rip on the daily?

async function main() {
  Promise.reject("uno");
  Promise.reject("dos");
  Promise.reject("tres");
  Promise.reject(new Error("the froggle is the wrong bodoozer"));
  (function gogogo () {
    Promise.reject(new Error("woops"));

    (function further() {
      Promise.reject(new Error("how many promises do I rip on the daily?"));
    }());
  }())

  await timeout(10000000);
  return true;
}
