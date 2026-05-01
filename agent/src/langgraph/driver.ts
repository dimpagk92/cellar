import type { Cel } from "../cel-bindings.js";
import type { PageContent } from "../types.js";

import type {
  CanonicalStep,
  CanonicalStepResult,
  PerceptionFrame,
} from "./canonical.js";

export interface CellarLangGraphDriver {
  perceive(options?: { captureScreenshot?: boolean }): Promise<PerceptionFrame>;
  executeStep(step: CanonicalStep): Promise<CanonicalStepResult>;
  getPageContent?(): Promise<PageContent | null>;
  readModel?(): { activeAdapters?: string[] | null } | null;
}

export class CelLangGraphDriver implements CellarLangGraphDriver {
  constructor(private readonly cel: Pick<
    Cel,
    | "canonicalPerceive"
    | "canonicalExecuteStep"
    | "getCdpPageContent"
    | "readCortexModel"
  >) {}

  perceive(options?: { captureScreenshot?: boolean }): Promise<PerceptionFrame> {
    return this.cel.canonicalPerceive(options?.captureScreenshot ?? true);
  }

  executeStep(step: CanonicalStep): Promise<CanonicalStepResult> {
    return this.cel.canonicalExecuteStep(step);
  }

  getPageContent(): Promise<PageContent | null> {
    return this.cel.getCdpPageContent();
  }

  readModel(): { activeAdapters?: string[] | null } | null {
    return this.cel.readCortexModel() as { activeAdapters?: string[] | null } | null;
  }
}
