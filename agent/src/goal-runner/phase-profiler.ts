/**
 * Lightweight phase-timing profiler for the goal-runner.
 *
 * Only active when CELLAR_PROFILE=1 or similar — zero cost otherwise.
 * Emits one JSON line per phase transition to stderr so it's trivial to
 * grep, aggregate, or pipe into a spreadsheet.
 *
 * Usage:
 *   const prof = PhaseProfiler.start(stepIndex);
 *   prof.mark("perceive");
 *   prof.mark("plan");
 *   ...
 *   prof.end();
 *
 * Output (example):
 *   {"lvl":"profile","step":0,"phase":"perceive","ms":53}
 *   {"lvl":"profile","step":0,"phase":"plan","ms":4820}
 *   {"lvl":"profile","step":0,"phase":"execute","ms":112}
 *   {"lvl":"profile","step":0,"phase":"total","ms":5120}
 */

const PROFILE_ENABLED =
  typeof process !== "undefined" &&
  (process.env.CELLAR_PROFILE === "1" || process.env.CELLAR_PROFILE === "true");

export class PhaseProfiler {
  private readonly enabled: boolean;
  private readonly step: number;
  private readonly t0: number;
  private lastMark: number;
  private lastPhaseName: string | null = null;

  private constructor(step: number, enabled: boolean) {
    this.enabled = enabled;
    this.step = step;
    this.t0 = enabled ? Date.now() : 0;
    this.lastMark = this.t0;
  }

  static start(step: number): PhaseProfiler {
    return new PhaseProfiler(step, PROFILE_ENABLED);
  }

  /** Record elapsed time since last mark under the previous phase name. */
  mark(phase: string): void {
    if (!this.enabled) return;
    const now = Date.now();
    if (this.lastPhaseName !== null) {
      const delta = now - this.lastMark;
      process.stderr.write(
        `{"lvl":"profile","step":${this.step},"phase":"${this.lastPhaseName}","ms":${delta}}\n`,
      );
    }
    this.lastPhaseName = phase;
    this.lastMark = now;
  }

  /** Close the profiler — flushes the final phase and emits a total. */
  end(): void {
    if (!this.enabled) return;
    const now = Date.now();
    if (this.lastPhaseName !== null) {
      const delta = now - this.lastMark;
      process.stderr.write(
        `{"lvl":"profile","step":${this.step},"phase":"${this.lastPhaseName}","ms":${delta}}\n`,
      );
    }
    const total = now - this.t0;
    process.stderr.write(
      `{"lvl":"profile","step":${this.step},"phase":"total","ms":${total}}\n`,
    );
  }

  /** Pre-flight timing helper — callable outside the step loop. */
  static preflight(name: string, start: number): void {
    if (!PROFILE_ENABLED) return;
    const delta = Date.now() - start;
    process.stderr.write(
      `{"lvl":"profile","step":"preflight","phase":"${name}","ms":${delta}}\n`,
    );
  }
}
