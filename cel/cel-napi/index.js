const { copyFileSync, existsSync, mkdirSync } = require("fs");
const { join } = require("path");
const { tmpdir } = require("os");

function stageAddon(sourcePath) {
  // On macOS, loading the addon directly from the workspace path can hang in
  // dyld. Staging through a fresh temp .node path avoids that.
  const stagedDir = join(tmpdir(), "cellar-napi");
  mkdirSync(stagedDir, { recursive: true });
  const suffix = Buffer.from(sourcePath).toString("hex").slice(-12);
  const stagedPath = join(stagedDir, `cel-napi.${process.pid}.${suffix}.node`);
  copyFileSync(sourcePath, stagedPath);
  return stagedPath;
}

function loadAddon(resolvedPath) {
  if (process.platform === "darwin") {
    return require(stageAddon(resolvedPath));
  }
  return require(resolvedPath);
}

const candidates = [];

if (process.env.CEL_NAPI_PATH) {
  candidates.push(process.env.CEL_NAPI_PATH);
}

// napi-rs naming conventions: <name>.<platform>-<arch>.node
candidates.push(
  join(__dirname, "cel-napi.darwin-arm64.node"),
  join(__dirname, "cel-napi.darwin-x64.node"),
  join(__dirname, "cel-napi.linux-x64-gnu.node"),
  join(__dirname, "cel-napi.linux-x64-musl.node"),
  join(__dirname, "cel-napi.linux-arm64-gnu.node"),
  join(__dirname, "cel-napi.win32-x64-msvc.node"),
);

const localPath = candidates.find((path) => existsSync(path));
if (localPath) {
  module.exports = loadAddon(localPath);
} else {
  const debugLinux = join(__dirname, "..", "..", "target", "debug", "libcel_napi.so");
  const releaseLinux = join(__dirname, "..", "..", "target", "release", "libcel_napi.so");
  const debugMac = join(__dirname, "..", "..", "target", "debug", "libcel_napi.dylib");
  const releaseMac = join(__dirname, "..", "..", "target", "release", "libcel_napi.dylib");
  const fallback = [debugLinux, releaseLinux, debugMac, releaseMac].find((path) =>
    existsSync(path),
  );

  if (!fallback) {
    throw new Error([
      "CEL native module not found.",
      "Build with: cargo build -p cel-napi",
      "Or copy compiled artifact to cel/cel-napi/cel-napi.<triple>.node.",
      `Checked:\n- ${candidates.join("\n- ")}`,
      `Fallback shared libs:\n- ${[debugLinux, releaseLinux, debugMac, releaseMac].join("\n- ")}`,
    ].join("\n"));
  }

  module.exports = loadAddon(fallback);
}
