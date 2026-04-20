#!/usr/bin/env node

import { startStdioServer } from "./server.js";

function printHelp(): void {
  console.error("CEL MCP Server");
  console.error("");
  console.error("Usage:");
  console.error("  cel-mcp               Start MCP server over stdio");
  console.error("  cel-mcp --help        Show this help");
}

if (process.argv.includes("--help") || process.argv.includes("-h")) {
  printHelp();
  process.exit(0);
}

startStdioServer().catch((err) => {
  console.error("Fatal:", err);
  process.exit(1);
});
