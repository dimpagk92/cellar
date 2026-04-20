import { build } from "esbuild";

await build({
  entryPoints: ["dist/index.js"],
  bundle: true,
  platform: "node",
  target: "node20",
  format: "cjs",
  outfile: "dist/bundle.cjs",
  external: ["@dimpagk92/cellar-napi"],
  banner: {
    js: "#!/usr/bin/env node",
  },
  logLevel: "info",
});
