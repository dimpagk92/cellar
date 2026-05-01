import type { ScreenContext } from "../types.js";

export interface RuntimeCaps {
  cdp_bound: boolean;
  cdp_browser?: string | null;
  cdp_url?: string | null;
  native_input: boolean;
  steps_used: number;
  max_steps: number;
}

export interface CellWrite {
  cell_ref: string;
  value: string;
}

export type CanonicalAction =
  | { type: "click"; target_id: string }
  | { type: "type"; target_id?: string | null; text: string }
  | { type: "key"; key: string }
  | { type: "key_combo"; keys: string[] }
  | { type: "set_value"; target_id: string; value: string }
  | { type: "scroll"; dx: number; dy: number }
  | { type: "drag"; from_target_id: string; to_target_id: string }
  | { type: "wait"; ms: number }
  | { type: "custom"; adapter: string; action: string; params?: unknown }
  | { type: "extract"; goal: string; data: string }
  | { type: "batch"; actions: CanonicalAction[] }
  | { type: "act"; instruction: string }
  | { type: "done"; summary: string; evidence_ids?: string[] }
  | { type: "fail"; reason: string }
  | { type: "ax_action"; target_id: string; action: string; label?: string | null; role_hint?: string | null }
  | { type: "activate_app"; app_name: string }
  | { type: "select"; from_x: number; from_y: number; to_x: number; to_y: number }
  | { type: "cdp_eval"; expression: string }
  | { type: "navigate"; url: string }
  | { type: "notebook_writes"; key?: string; value?: string; category?: string }
  | { type: "extract_with_fallback"; name: string; selectors: string[]; parse_as?: string }
  | { type: "write_cells"; app?: string; sheet?: string | null; table?: string | null; writes: CellWrite[]; verify?: boolean }
  | { type: "read_cells"; app?: string; sheet?: string | null; table?: string | null; cell_refs: string[] };

export type CanonicalStepKind = "deterministic" | "llm_assisted";

export interface CanonicalStep {
  purpose: string;
  kind: CanonicalStepKind;
  action: CanonicalAction;
}

export interface AttemptRecord {
  step_purpose: string;
  action: CanonicalAction;
  succeeded: boolean;
  error?: string | null;
  data: unknown;
}

export type NextMove =
  | {
      kind: "batch";
      purpose: string;
      steps: CanonicalStep[];
    }
  | {
      kind: "done";
      summary: string;
      extracted_data?: unknown;
    }
  | {
      kind: "fail";
      reason: string;
    };

export type CanonicalStepResult =
  | {
      status: "ok";
      data?: unknown;
      discovered_sub_goal?: unknown;
    }
  | {
      status: "err";
      message: string;
      recoverable?: boolean;
    };

export interface DoneVerdict {
  verified: boolean;
  reason: string;
}

export interface FailureReport {
  failing_sub_goal: string;
  failing_step: string;
  attempts: string[];
}

export type GoalOutcome =
  | {
      status: "succeeded";
      summary: string;
      extracted_data?: unknown;
    }
  | ({
      status: "failed";
    } & FailureReport);

export interface PerceptionFrame {
  perception: ScreenContext;
  screenshot_base64?: string | null;
  caps: RuntimeCaps;
}

export interface ReviewDecision {
  approved: boolean;
  edited_step?: CanonicalStep;
  feedback?: string;
}
