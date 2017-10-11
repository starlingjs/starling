// The task stops after `main` exits and then we flush the micro-task queue. Any
// promise that hasn't settled is abandoned. Therefore, the error after the
// timeout will not trigger our unhandled-rejected-promises task killing because
// it will never execut and throw.

async function main() {
  // No await.
  timeout(1).then(_ => {
    throw new Error();
  });
}
