import { Command } from "commander";
import { Cel, type CanonicalStep, type ContextElement, type PerceptionFrame, type ScreenContext } from "@cellar/agent";

interface NumbersSmokeOptions {
  write?: string[];
  sheet?: string;
  table?: string;
  json?: boolean;
  stepTimeoutMs: number;
  requireAxVisible?: boolean;
}

interface CellWrite {
  cell_ref: string;
  value: string;
}

const DEFAULT_WRITES: CellWrite[] = [
  { cell_ref: "A1", value: "BTC" },
  { cell_ref: "B1", value: "ETH" },
  { cell_ref: "C1", value: "SOL" },
];

function parseWriteSpec(spec: string): CellWrite {
  const idx = spec.indexOf("=");
  if (idx <= 0 || idx === spec.length - 1) {
    throw new Error(`Invalid --write value "${spec}". Use A1=BTC form.`);
  }
  return {
    cell_ref: spec.slice(0, idx).trim(),
    value: spec.slice(idx + 1).trim(),
  };
}

function summarizeFrame(frame: PerceptionFrame) {
  const ctx = frame.perception;
  const focused = ctx.elements.find((el) => el.state?.focused);
  return {
    app: ctx.app,
    window: ctx.window,
    elements: ctx.elements.length,
    focused_element: focused
      ? {
          id: focused.id,
          label: focused.label ?? null,
          value: focused.value ?? null,
          type: focused.element_type,
        }
      : null,
    caps: frame.caps,
  };
}

function buildAxHaystack(ctx: ScreenContext): string {
  const parts: string[] = [];
  const push = (value: unknown) => {
    if (typeof value !== "string") return;
    const trimmed = value.trim();
    if (trimmed.length > 0) parts.push(trimmed);
  };

  push(ctx.app);
  push(ctx.window);
  for (const window of ctx.window_list ?? []) {
    push(window.app_name);
    push(window.title);
  }
  for (const app of ctx.running_apps ?? []) {
    push(app.name);
  }
  for (const el of ctx.elements) {
    push(el.label);
    push(el.value);
    push(el.description);
    if (el.properties) {
      for (const value of Object.values(el.properties)) {
        push(value);
      }
    }
  }
  return parts.join("\n").toLowerCase();
}

function findLabeledButton(ctx: ScreenContext, candidates: string[]): ContextElement | undefined {
  const lowered = candidates.map((candidate) => candidate.toLowerCase());
  return ctx.elements.find((el) => {
    if (el.element_type !== "button" && el.element_type !== "link") return false;
    const label = (el.label ?? el.value ?? "").trim().toLowerCase();
    return label.length > 0 && lowered.some((candidate) => label.includes(candidate));
  });
}

function summarizeActionables(ctx: ScreenContext): Array<{ type: string; label: string }> {
  return ctx.elements
    .filter((el) => {
      const label = (el.label ?? el.value ?? "").trim();
      return label.length > 0 && ["button", "link", "combobox", "tree_view", "list"].includes(el.element_type);
    })
    .slice(0, 20)
    .map((el) => ({
      type: el.element_type,
      label: (el.label ?? el.value ?? "").trim(),
    }));
}

function extractNativePreviewCells(ctx: ScreenContext): Array<{ ref: string; value: string }> {
  return ctx.elements
    .filter((el) => (el.id ?? "").startsWith("numbers:cell:"))
    .map((el) => ({
      ref: el.properties?.cell_ref ?? el.label ?? el.id.replace("numbers:cell:", ""),
      value: el.value ?? "",
    }))
    .filter((entry) => entry.value.trim().length > 0)
    .sort((a, b) => a.ref.localeCompare(b.ref));
}

async function settleCortex(cel: Cel): Promise<void> {
  try {
    await cel.cortexRefreshNow(1500);
  } catch {
    // Best effort refresh only; some runs are still useful without it.
  }
  await new Promise((resolve) => setTimeout(resolve, 500));
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<T>((_, reject) => {
        timer = setTimeout(() => {
          reject(new Error(`${label} timed out after ${timeoutMs}ms`));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function runCanonicalStep(cel: Cel, step: CanonicalStep) {
  return cel.canonicalExecuteStep(step);
}

async function ensureNumbersDocument(
  cel: Cel,
  frame: PerceptionFrame,
  timeoutMs: number,
): Promise<{ frame: PerceptionFrame; bootstrap?: string }> {
  let current = frame;
  const windowTitle = (current.perception.window ?? "").trim().toLowerCase();
  if (windowTitle !== "open") {
    return { frame: current };
  }

  const bootstrapButton = findLabeledButton(current.perception, [
    "new document",
    "create document",
    "blank",
    "blank document",
  ]);

  if (bootstrapButton) {
    await withTimeout(runCanonicalStep(cel, {
      purpose: "Dismiss the Numbers open dialog into a blank document",
      kind: "deterministic",
      action: {
        type: "ax_action",
        target_id: bootstrapButton.id,
        action: "click",
        label: bootstrapButton.label ?? undefined,
        role_hint: bootstrapButton.element_type,
      },
    }), timeoutMs, "bootstrap Numbers document");
    await settleCortex(cel);
    current = await withTimeout(cel.canonicalPerceive(false), timeoutMs, "post-bootstrap perceive");
    if ((current.perception.window ?? "").trim().toLowerCase() !== "open") {
      return { frame: current, bootstrap: `clicked ${bootstrapButton.label ?? bootstrapButton.id}` };
    }

    await withTimeout(runCanonicalStep(cel, {
      purpose: "Confirm the Numbers new document dialog with Enter",
      kind: "deterministic",
      action: {
        type: "key",
        key: "Enter",
      },
    }), timeoutMs, "Numbers Enter bootstrap");
    await settleCortex(cel);
    current = await withTimeout(cel.canonicalPerceive(false), timeoutMs, "post-Enter perceive");
    if ((current.perception.window ?? "").trim().toLowerCase() !== "open") {
      return { frame: current, bootstrap: `clicked ${bootstrapButton.label ?? bootstrapButton.id}, then pressed Enter` };
    }
  }

  await withTimeout(runCanonicalStep(cel, {
    purpose: "Create a new Numbers document from the keyboard",
    kind: "deterministic",
    action: {
      type: "key_combo",
      keys: ["Cmd", "N"],
    },
  }), timeoutMs, "Numbers Cmd+N bootstrap");
  await settleCortex(cel);
  current = await withTimeout(cel.canonicalPerceive(false), timeoutMs, "post-Cmd+N perceive");
  return { frame: current, bootstrap: bootstrapButton ? "clicked dialog action, then sent Cmd+N" : "sent Cmd+N" };
}

export const numbersSmokeCommand = new Command("numbers-smoke")
  .description("Direct Cortex smoke test: activate Numbers, write cells via write_cells, read them back from the document model, and compare with AX state")
  .option(
    "--write <cell=value>",
    "Cell write to perform (repeatable). Defaults to A1=BTC B1=ETH C1=SOL",
    (value: string, acc: string[]) => {
      acc.push(value);
      return acc;
    },
    [],
  )
  .option("--sheet <name>", "Numbers sheet name override")
  .option("--table <name>", "Numbers table name override")
  .option("--step-timeout-ms <n>", "Timeout per canonical perceive/execute call", (value: string) => parseInt(value, 10), 20_000)
  .option("--require-ax-visible", "Fail unless all requested values are visible in the post-write AX/context view", false)
  .option("--json", "Output raw JSON", false)
  .action(async (opts: NumbersSmokeOptions) => {
    const cel = new Cel();
    let bootedHere = false;

    if (!cel.isNativeAvailable) {
      console.error(
        "CEL native module not available. Build it with `cargo build -p cel-napi --release` and copy the .node file into cel/cel-napi/.",
      );
      process.exit(1);
    }

    const writes = (opts.write?.length ? opts.write.map(parseWriteSpec) : DEFAULT_WRITES);

    if (!cel.isCortexRunning()) {
      console.error("Booting Cortex...");
      cel.bootCortex();
      bootedHere = true;
      await new Promise((resolve) => setTimeout(resolve, 700));
    }

    try {
      const before = await withTimeout(cel.canonicalPerceive(false), opts.stepTimeoutMs, "initial perceive");
      const activateResult = await withTimeout(runCanonicalStep(cel, {
        purpose: "Bring Numbers to the foreground",
        kind: "deterministic",
        action: {
          type: "activate_app",
          app_name: "Numbers",
        },
      }), opts.stepTimeoutMs, "activate_app(Numbers)");

      await settleCortex(cel);
      const afterActivateRaw = await withTimeout(cel.canonicalPerceive(false), opts.stepTimeoutMs, "post-activate perceive");
      const bootstrap = await ensureNumbersDocument(cel, afterActivateRaw, opts.stepTimeoutMs);
      const afterActivate = bootstrap.frame;

      const writeAction: CanonicalStep["action"] = {
        type: "write_cells",
        app: "Numbers",
        writes,
        verify: true,
      };
      if (opts.sheet) writeAction.sheet = opts.sheet;
      if (opts.table) writeAction.table = opts.table;

      const writeResult = await withTimeout(runCanonicalStep(cel, {
        purpose: "Write requested values into Numbers cells",
        kind: "deterministic",
        action: writeAction,
      }), opts.stepTimeoutMs, "write_cells(Numbers)");

      const readResult = await withTimeout(runCanonicalStep(cel, {
        purpose: "Read the requested Numbers cells back from the document model",
        kind: "deterministic",
        action: {
          type: "read_cells",
          app: "Numbers",
          sheet: opts.sheet,
          table: opts.table,
          cell_refs: writes.map((write) => write.cell_ref),
        },
      }), opts.stepTimeoutMs, "read_cells(Numbers)");

      await settleCortex(cel);
      const afterWrite = await withTimeout(cel.canonicalPerceive(false), opts.stepTimeoutMs, "post-write perceive");
      const model = cel.readCortexModel() as {
        active_adapters?: string[];
        element_adapter_index?: Record<string, string>;
      };
      const haystack = buildAxHaystack(afterWrite.perception);
      const visibleWrites = writes.filter((write) => haystack.includes(write.value.toLowerCase()));
      const missingValues = writes
        .filter((write) => !visibleWrites.includes(write))
        .map((write) => write.value);
      const modelReads =
        readResult.status === "ok" && readResult.data && typeof readResult.data === "object"
          ? (readResult.data as { reads?: Array<{ ref?: string; value?: string }> }).reads ?? []
          : [];
      const nativePreviewCells = extractNativePreviewCells(afterWrite.perception);

      const result = {
        writes,
        activate_result: activateResult,
        write_result: writeResult,
        read_result: readResult,
        bootstrap_action: bootstrap.bootstrap ?? null,
        before: summarizeFrame(before),
        after_activate: summarizeFrame(afterActivate),
        after_activate_actionables: summarizeActionables(afterActivate.perception),
        after_write: summarizeFrame(afterWrite),
        active_adapters: model.active_adapters ?? [],
        adapter_backed_element_ids: Object.keys(model.element_adapter_index ?? {}).slice(0, 40),
        native_preview_cells: nativePreviewCells,
        model_reads: modelReads,
        visible_values: visibleWrites.map((write) => write.value),
        all_values_visible: visibleWrites.length === writes.length,
      };

      if (opts.json) {
        console.log(JSON.stringify(result, null, 2));
      } else {
        console.log("");
        console.log("=== Numbers Smoke ===");
        console.log(`Writes:   ${writes.map((write) => `${write.cell_ref}=${write.value}`).join("  ")}`);
        console.log(`Before:   ${result.before.app} - ${result.before.window || "(no window)"}`);
        console.log(`Activated:${result.after_activate.app} - ${result.after_activate.window || "(no window)"}`);
        console.log(`After:    ${result.after_write.app} - ${result.after_write.window || "(no window)"}`);
        console.log(`Activate: ${activateResult.status}`);
        if (result.bootstrap_action) {
          console.log(`Bootstrap:${result.bootstrap_action}`);
        }
        if (result.after_activate_actionables.length > 0 && result.after_activate.window === "Open") {
          console.log(`Dialog:   ${result.after_activate_actionables.map((item) => `${item.type}:${item.label}`).join(" | ")}`);
        }
        console.log(`Write:    ${writeResult.status}`);
        console.log(`Read:     ${readResult.status}`);
        if ((result.active_adapters ?? []).length > 0) {
          console.log(`Adapters: ${(result.active_adapters ?? []).join(", ")}`);
        }
        if (writeResult.status === "err") {
          console.log(`Error:    ${writeResult.message}`);
        }
        if (readResult.status === "ok" && modelReads.length > 0) {
          console.log(`Model:    ${modelReads.map((read) => `${read.ref}=${read.value ?? ""}`).join("  ")}`);
        }
        if (nativePreviewCells.length > 0) {
          console.log(`Preview:  ${nativePreviewCells.map((cell) => `${cell.ref}=${cell.value}`).join("  ")}`);
        }
        console.log(
          `AX text:  ${result.all_values_visible ? "all requested values visible" : `missing ${missingValues.join(", ")}`}`,
        );
      }

      if (
        activateResult.status !== "ok"
        || writeResult.status !== "ok"
        || readResult.status !== "ok"
        || (opts.requireAxVisible && !result.all_values_visible)
      ) {
        process.exit(1);
      }
    } catch (err) {
      console.error(`Numbers smoke failed: ${err instanceof Error ? err.message : err}`);
      process.exit(1);
    } finally {
      if (bootedHere && cel.isCortexRunning()) {
        try {
          cel.stopCortex();
        } catch {
          // Best effort only.
        }
      }
    }
  });
