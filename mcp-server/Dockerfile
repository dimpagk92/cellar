# Cellar MCP server — schema-only Docker image for Glama inspection.
#
# This image runs the MCP server WITHOUT the cel-napi native module.
# Tool schemas are exposed for discovery / scoring; real tool calls
# return an error pointing at the macOS install path.
#
# For a working install, run on macOS — see github.com/dimpagk92/cellar.

FROM node:22-slim AS builder
WORKDIR /repo

# Install pnpm
RUN corepack enable && corepack prepare pnpm@9.15.4 --activate

# Copy the entire repo (sync-to-oss already excluded irrelevant files)
COPY . ./

# Install JS deps — skip native build scripts (cel-napi requires Rust toolchain)
RUN pnpm install --frozen-lockfile --ignore-scripts

# Build (agent → adapter-browser → mcp-server)
RUN pnpm --filter @cellar/agent build \
  && pnpm --filter @cellar/adapter-browser build \
  && pnpm --filter @dpagk/cellar-mcp exec tsc

# ─── Runtime stage ──────────────────────────────────────────────────────────
FROM node:22-slim
WORKDIR /app

# Copy built workspace (node_modules + dist for each package)
COPY --from=builder /repo/node_modules ./node_modules
COPY --from=builder /repo/agent ./agent
COPY --from=builder /repo/adapters/browser ./adapters/browser
COPY --from=builder /repo/mcp-server ./mcp-server

WORKDIR /app/mcp-server
CMD ["node", "dist/index.js"]
