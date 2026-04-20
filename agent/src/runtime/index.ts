/**
 * Runtime Kernel — public API.
 */

export { executePlannedAction, verifyActionOutcome } from "./kernel.js";
export type {
  AdapterCapabilities,
  KernelActionOutcome,
  KernelExecutionInput,
  KernelEvent,
  KernelEventType,
  VerificationResult,
} from "./types.js";
export { toActionOutcome } from "./types.js";
export {
  AdapterRegistry,
  type AdapterInstance,
  type AdapterManifest,
  type AdapterState,
  type AdapterPlatform,
} from "./adapter-registry.js";
