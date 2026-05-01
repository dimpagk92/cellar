import { promises as fs } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const OBS_DIR = join(homedir(), ".cellar", "observations");
const MAX_KEEP = 500;
const OBSERVATION_ID_RE = /^obs_\d{13}_[0-9a-f]{8}$/;

let ensuredDir = false;

async function ensureDir(): Promise<void> {
  if (ensuredDir) return;
  await fs.mkdir(OBS_DIR, { recursive: true });
  ensuredDir = true;
}

function shortHash(s: string): string {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

/**
 * Persist a full observation snapshot to ~/.cellar/observations/ and return an id.
 *
 * Fire-and-forget: the response is not blocked on disk IO. If writing fails, the
 * observation_id is still valid (the disk copy just won't be there) — the inline
 * response is the source of truth for the current turn.
 */
export function persistObservation(data: unknown): string {
  const ts = Date.now();
  const digest = shortHash(JSON.stringify(data).slice(0, 4096));
  const id = `obs_${ts}_${digest}`;
  const path = join(OBS_DIR, `${id}.json`);

  void (async () => {
    try {
      await ensureDir();
      await fs.writeFile(path, JSON.stringify(data, null, 2), "utf8");
      await pruneOld();
    } catch {
      // Best-effort — persistence is non-critical.
    }
  })();

  return id;
}

/** Keep only the newest MAX_KEEP observations. */
async function pruneOld(): Promise<void> {
  try {
    const entries = await fs.readdir(OBS_DIR);
    const files = entries.filter((f) => f.startsWith("obs_") && f.endsWith(".json"));
    if (files.length <= MAX_KEEP) return;
    files.sort(); // filenames start with timestamp, so lexicographic = chronological
    const toDelete = files.slice(0, files.length - MAX_KEEP);
    await Promise.all(
      toDelete.map((f) => fs.unlink(join(OBS_DIR, f)).catch(() => undefined)),
    );
  } catch {
    // Ignore prune failures.
  }
}

/** Read a previously persisted observation by id. Returns null if missing. */
export async function readObservation(id: string): Promise<unknown | null> {
  if (!OBSERVATION_ID_RE.test(id)) {
    return null;
  }
  try {
    await ensureDir();
    const path = join(OBS_DIR, `${id}.json`);
    const data = await fs.readFile(path, "utf8");
    return JSON.parse(data);
  } catch {
    return null;
  }
}
