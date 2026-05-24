import { build } from "esbuild";

await build({
  entryPoints: ["dist/index.js"],
  bundle: true,
  platform: "node",
  target: "node20",
  format: "cjs",
  outfile: "dist/bundle.cjs",
  // Native .node binaries (cel-napi) can't be bundled; copy them next to
  // the output so the require() path inside cel-napi/index.js still resolves.
  loader: { ".node": "copy" },
  external: [
    "@dimpagk92/cellar-napi",
    // playwright-core uses require.resolve() with relative paths that
    // esbuild can't statically follow. Marking it external sidesteps the
    // require-resolve-not-external warnings without changing runtime.
    "playwright-core",
  ],
  banner: {
    js: "#!/usr/bin/env node",
  },
  logLevel: "info",
});
