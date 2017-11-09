//# starling-stdout-has: 10.js: spawned
//# starling-stdout-has: 9.js: spawned
//# starling-stdout-has: 8.js: spawned
//# starling-stdout-has: 7.js: spawned
//# starling-stdout-has: 6.js: spawned
//# starling-stdout-has: 5.js: spawned
//# starling-stdout-has: 4.js: spawned
//# starling-stdout-has: 3.js: spawned
//# starling-stdout-has: 2.js: spawned
//# starling-stdout-has: 1.js: spawned
//# starling-stdout-has: 0.js: spawned and done
//# starling-stdout-has: 1.js: done
//# starling-stdout-has: 2.js: done
//# starling-stdout-has: 3.js: done
//# starling-stdout-has: 4.js: done
//# starling-stdout-has: 5.js: done
//# starling-stdout-has: 6.js: done
//# starling-stdout-has: 7.js: done
//# starling-stdout-has: 8.js: done
//# starling-stdout-has: 9.js: done
//# starling-stdout-has: 10.js: done

async function main() {
  const then = Date.now();
  await spawn("./spawn-and-wait/10.js");
  const now = Date.now();
  print("Time: ", now - then, " ms");
}
