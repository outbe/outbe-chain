// User Prompts
// Functions for pausing execution and waiting for user input.

export function promptForContinue(message: string): Promise<void> {
  return new Promise((resolve) => {
    process.stdout.write(`\n${message}\nPress Enter to continue...`);
    process.stdin.setRawMode(false);
    process.stdin.resume();
    process.stdin.setEncoding("utf8");

    const onData = (key: Buffer) => {
      if (key.toString() === "\n" || key.toString() === "\r") {
        process.stdin.removeListener("data", onData);
        process.stdin.pause();
        process.stdout.write("\n");
        resolve();
      }
    };

    process.stdin.once("data", onData);
  });
}

/**
 * Wait for Enter key without displaying a message.
 */
export function waitForEnter(): Promise<void> {
  return new Promise((resolve) => {
    process.stdin.setRawMode(false);
    process.stdin.resume();
    process.stdin.setEncoding("utf8");

    const onData = (key: Buffer) => {
      if (key.toString() === "\n" || key.toString() === "\r") {
        process.stdin.removeListener("data", onData);
        process.stdin.pause();
        resolve();
      }
    };

    process.stdin.once("data", onData);
  });
}
