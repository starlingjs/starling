//# starling-stdout-has: 1
//# starling-stdout-has: 2
//# starling-stdout-has: 3
//# starling-stdout-has: 4
//# starling-stdout-has: 5
//# starling-stdout-has: 6
//# starling-stdout-has: 7
//# starling-stdout-has: 9

print("1");

async function delayedError() {
  await Promise.resolve();
  throw new Error();
}

delayedError().catch(_ => {});
delayedError().catch(_ => {});
delayedError().catch(_ => {});
delayedError().catch(_ => {});

(async function () {
  print("2");
  await Promise.resolve();
  print("3");
  await Promise.resolve();
  print("4");
  await Promise.resolve();
  print("5");
  await Promise.resolve();
  print("6");
  await Promise.resolve();
  print("7");
  throw new Error();
  print("8");
}()).catch(_ => {});

print("9");
