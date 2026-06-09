// Argument Parser
// Parse command line arguments for standalone scripts.
// Supports --key value and --key=value formats

export function parseArgs(): Record<string, string> {
  const args = process.argv.slice(2);
  const params: Record<string, string> = {};

  for (let i = 0; i < args.length; i++) {
    if (!args[i].startsWith("--")) continue;

    const arg = args[i].slice(2);

    // Handle --key=value format
    if (arg.includes("=")) {
      const [key, value] = arg.split("=");
      params[key] = value;
      continue;
    }

    // Handle --key value format
    const value = args[i + 1] && !args[i + 1].startsWith("--") ? args[i + 1] : "";
    params[arg] = value;
    if (value) i++;
  }

  return params;
}
