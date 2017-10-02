print("1");

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
  await Promise.resolve();
  print("8");
}());

print("9");
