async function main() {
  // Things that should structure clone OK.

  let undef = await spawn("./task-resolves/undefined.js");
  if (undef !== undefined) {
    throw new Error("task resolves to undefined");
  }

  let n = await spawn("./task-resolves/number.js");
  if (n !== 42) {
    throw new Error("task resolves to number");
  }

  let s = await spawn("./task-resolves/string.js");
  if (s !== "hello") {
    throw new Error("task resolves to string");
  }

  let o = await spawn("./task-resolves/object.js");
  let keys = Object.keys(o);
  if (keys.length !== 1) {
    throw new Error("task resolves to object: keys.length");
  }
  if (keys[0] !== "pinball") {
    throw new Error("task resolves to object: keys[0]");
  }
  if (o.pinball !== "wizard") {
    throw new Error("task resolves to object: o.pinball");
  }

  // Things that should fail to structure clone, raising an error.

  let error;

  error = null;
  try {
    await spawn("./task-resolves/function.js");
  } catch(e) {
    error = e;
  }
  if (error === null) {
    throw new Error("task resolves to function did not throw an error");
  }

  error = null;
  try {
    await spawn("./task-resolves/symbol.js");
  } catch(e) {
    error = e;
  }
  if (error === null) {
    throw new Error("task resolves to symbol did not throw an error");
  }

  error = null;
  try {
    await spawn("./task-resolves/symbol-for.js");
  } catch (e) {
    error = e;
  }
  if (error === null) {
    throw new Error("task resolve to Symbol.for(..) symbol did not throw an error");
  }
}
