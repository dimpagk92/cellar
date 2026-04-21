import { Command } from "commander";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs/promises";

type ClientId =
  | "claude-desktop"
  | "claude-code"
  | "cursor"
  | "windsurf"
  | "zed";

const CLIENTS: ClientId[] = [
  "claude-desktop",
  "claude-code",
  "cursor",
  "windsurf",
  "zed",
];

/** Where each client stores its MCP config. Returns an absolute path. */
function configPath(client: ClientId): string {
  const home = os.homedir();
  const platform = process.platform;

  switch (client) {
    case "claude-desktop":
      if (platform === "darwin") {
        return path.join(
          home,
          "Library/Application Support/Claude/claude_desktop_config.json"
        );
      }
      if (platform === "win32") {
        return path.join(
          process.env.APPDATA ?? path.join(home, "AppData/Roaming"),
          "Claude/claude_desktop_config.json"
        );
      }
      return path.join(home, ".config/Claude/claude_desktop_config.json");
    case "claude-code":
      return path.join(home, ".claude.json");
    case "cursor":
      return path.join(home, ".cursor/mcp.json");
    case "windsurf":
      return path.join(home, ".codeium/windsurf/mcp_config.json");
    case "zed":
      if (platform === "darwin") {
        return path.join(home, ".config/zed/settings.json");
      }
      return path.join(home, ".config/zed/settings.json");
  }
}

/**
 * Clients that nest MCP servers differently. Most use `{ mcpServers: {...} }`;
 * Zed uses `{ context_servers: {...} }` inside a larger settings file.
 */
function configShape(client: ClientId): "mcpServers" | "context_servers" {
  return client === "zed" ? "context_servers" : "mcpServers";
}

type McpEntry = {
  command: string;
  args?: string[];
  env?: Record<string, string>;
};

function serverEntry(nodePath?: string): McpEntry {
  if (nodePath) {
    return { command: "node", args: [nodePath] };
  }
  return { command: "npx", args: ["-y", "@cellar/cli", "mcp"] };
}

async function readJson(p: string): Promise<Record<string, unknown>> {
  try {
    const raw = await fs.readFile(p, "utf8");
    return JSON.parse(raw) as Record<string, unknown>;
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return {};
    throw e;
  }
}

async function writeJson(p: string, data: unknown) {
  await fs.mkdir(path.dirname(p), { recursive: true });
  await fs.writeFile(p, JSON.stringify(data, null, 2) + "\n", "utf8");
}

async function backupIfExists(p: string): Promise<string | null> {
  try {
    await fs.access(p);
  } catch {
    return null;
  }
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const backup = `${p}.${stamp}.bak`;
  await fs.copyFile(p, backup);
  return backup;
}

async function installFor(
  client: ClientId,
  serverName: string,
  entry: McpEntry,
  opts: { dryRun: boolean; force: boolean }
): Promise<{ path: string; action: "created" | "updated" | "skipped"; backup?: string | null }> {
  const p = configPath(client);
  const shape = configShape(client);

  const existing = await readJson(p);
  const nested = (existing[shape] as Record<string, McpEntry> | undefined) ?? {};

  if (nested[serverName] && !opts.force) {
    return { path: p, action: "skipped" };
  }

  const next = {
    ...existing,
    [shape]: { ...nested, [serverName]: entry },
  };

  if (opts.dryRun) {
    return { path: p, action: nested[serverName] ? "updated" : "created" };
  }

  const backup = await backupIfExists(p);
  await writeJson(p, next);
  return {
    path: p,
    action: nested[serverName] ? "updated" : "created",
    backup,
  };
}

export const mcpCommand = new Command("mcp")
  .description("Start the CEL MCP server for Claude Desktop / Cursor integration")
  .option("--sse", "Use SSE transport instead of stdio")
  .option("--port <port>", "Port for SSE transport", "3100")
  .action(async (opts) => {
    if (opts.sse) {
      console.error(
        `SSE transport not yet implemented. Use stdio (default) for now.`
      );
      process.exit(1);
    }
    const { startStdioServer } = await import("@dpagk/cellar-mcp/server.js");
    await startStdioServer();
  })
  .addCommand(
    new Command("install")
      .description(
        "Write CEL MCP server config into Claude Desktop, Cursor, Claude Code, Windsurf, or Zed."
      )
      .option(
        "-c, --client <client>",
        `MCP client to configure. One of: ${CLIENTS.join(", ")}. Omit to install into all detected clients.`
      )
      .option(
        "--node-path <path>",
        "Use 'node <path>' as the command instead of 'npx -y @cellar/cli mcp'. Useful during development."
      )
      .option("--name <name>", "Server name in the config", "cel")
      .option("--force", "Overwrite an existing entry with the same name", false)
      .option("--dry-run", "Show what would change without writing anything", false)
      .option("--print", "Print the snippet instead of writing — no files touched", false)
      .action(async (opts) => {
        const entry = serverEntry(opts.nodePath);
        const name = opts.name as string;

        if (opts.print) {
          const payload = { mcpServers: { [name]: entry } };
          console.log(JSON.stringify(payload, null, 2));
          return;
        }

        const targets: ClientId[] =
          opts.client != null
            ? [opts.client as ClientId]
            : CLIENTS;

        if (opts.client != null && !CLIENTS.includes(opts.client as ClientId)) {
          console.error(
            `Unknown client '${opts.client}'. Supported: ${CLIENTS.join(", ")}`
          );
          process.exit(1);
        }

        let installed = 0;
        let skipped = 0;

        for (const client of targets) {
          try {
            const result = await installFor(client, name, entry, {
              dryRun: opts.dryRun,
              force: opts.force,
            });
            const tag = opts.dryRun ? "[dry-run] " : "";
            if (result.action === "skipped") {
              console.log(
                `${tag}skipped ${client} — entry '${name}' already exists at ${result.path} (use --force to overwrite)`
              );
              skipped += 1;
            } else {
              console.log(
                `${tag}${result.action} ${client} at ${result.path}` +
                  (result.backup ? ` (backup: ${result.backup})` : "")
              );
              installed += 1;
            }
          } catch (e) {
            const msg = e instanceof Error ? e.message : String(e);
            console.error(`failed ${client}: ${msg}`);
          }
        }

        if (!opts.dryRun) {
          console.log(
            `\n${installed} installed, ${skipped} skipped. Restart the client(s) to pick up the new server.`
          );
        }
      })
  )
  .addCommand(
    new Command("uninstall")
      .description("Remove the CEL MCP server entry from one or all clients.")
      .option(
        "-c, --client <client>",
        `MCP client to clean. One of: ${CLIENTS.join(", ")}. Omit for all.`
      )
      .option("--name <name>", "Server name to remove", "cel")
      .option("--dry-run", "Show what would change without writing anything", false)
      .action(async (opts) => {
        const name = opts.name as string;
        const targets: ClientId[] =
          opts.client != null ? [opts.client as ClientId] : CLIENTS;

        if (opts.client != null && !CLIENTS.includes(opts.client as ClientId)) {
          console.error(
            `Unknown client '${opts.client}'. Supported: ${CLIENTS.join(", ")}`
          );
          process.exit(1);
        }

        for (const client of targets) {
          const p = configPath(client);
          const shape = configShape(client);
          const existing = await readJson(p);
          const nested =
            (existing[shape] as Record<string, unknown> | undefined) ?? {};
          if (!(name in nested)) {
            console.log(`not present in ${client} (${p})`);
            continue;
          }
          delete nested[name];
          const next = { ...existing, [shape]: nested };
          if (opts.dryRun) {
            console.log(`[dry-run] would remove '${name}' from ${p}`);
            continue;
          }
          const backup = await backupIfExists(p);
          await writeJson(p, next);
          console.log(
            `removed '${name}' from ${client} at ${p}` +
              (backup ? ` (backup: ${backup})` : "")
          );
        }
      })
  )
  .addCommand(
    new Command("status")
      .description("Show where the CEL MCP server is currently registered.")
      .option("--name <name>", "Server name to look for", "cel")
      .action(async (opts) => {
        const name = opts.name as string;
        for (const client of CLIENTS) {
          const p = configPath(client);
          try {
            await fs.access(p);
          } catch {
            console.log(`${client.padEnd(16)} — config not found (${p})`);
            continue;
          }
          const data = await readJson(p);
          const nested =
            (data[configShape(client)] as
              | Record<string, unknown>
              | undefined) ?? {};
          const present = name in nested ? "installed" : "not installed";
          console.log(`${client.padEnd(16)} — ${present} (${p})`);
        }
      })
  );
