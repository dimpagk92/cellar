#!/usr/bin/env tsx
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const SERVER = process.env.CEL_MCP_SERVER;
if (!SERVER) {
  console.error("set CEL_MCP_SERVER to /absolute/path/to/cellar/mcp-server/dist/index.js");
  process.exit(1);
}

const log = (msg: string) => console.log(`[cel-demo] ${msg}`);

async function main() {
  log("connecting...");
  const transport = new StdioClientTransport({ command: "node", args: [SERVER] });
  const client = new Client({ name: "cel-demo", version: "0.1.0" }, { capabilities: {} });
  await client.connect(transport);

  const { tools } = await client.listTools();
  log(`connected. tools: ${tools.map((t) => t.name).join(", ")}`);

  const windowsResult = await client.callTool({
    name: "cel_see",
    arguments: { mode: "windows" },
  });
  const windows = parseToolJson<{ id: string; title: string; app: string }[]>(windowsResult);
  log(`cel_see windows -> ${windows?.length ?? 0} visible windows`);
  for (const w of (windows ?? []).slice(0, 5)) {
    log(`  • ${w.app} — ${w.title}`);
  }

  const screenshotResult = await client.callTool({
    name: "cel_see",
    arguments: { mode: "screenshot" },
  });
  const screenshot = parseToolJson<{ image_base64: string }>(screenshotResult);
  const bytes = screenshot?.image_base64 ? Buffer.from(screenshot.image_base64, "base64").length : 0;
  log(`cel_see screenshot -> ${bytes} bytes`);

  await client.close();
  log("disconnected.");
}

function parseToolJson<T>(result: unknown): T | null {
  const content = (result as { content?: { type: string; text?: string }[] })?.content?.[0];
  if (!content || content.type !== "text" || !content.text) return null;
  try {
    return JSON.parse(content.text) as T;
  } catch {
    return null;
  }
}

main().catch((err) => {
  console.error("[cel-demo] error:", err?.message ?? err);
  process.exit(1);
});
