/**
 * @cellar/agent/runtime — the primitive surface every agent backend consumes.
 *
 * This barrel is the contract between CEL (the platform) and any agent that
 * wants to drive it: the MCP server (Claude Code, Cursor, Codex), the
 * in-process LangGraph driver, the built-in runGoal loop, future Mastra
 * integration, or a custom in-house planner.
 *
 * Rule of thumb:
 *   - "Anything an agent backend needs to perceive the screen, execute an
 *     action, manage knowledge, and run a Cortex session" lives here.
 *   - "The built-in TypeScript planner/runner itself" (WorkflowEngine,
 *     runGoal, orchestrator, LangGraph driver, replay, strategy router,
 *     self-healer, etc.) lives at the @cellar/agent root and is OPT-IN.
 *
 * Boundary rule for new agent backends:
 *   An agent backend depends on `@cellar/agent/runtime` and nothing else
 *   from `@cellar/agent`. If you find yourself wanting to import a planner
 *   helper, either promote it here or vendor it inside your backend.
 */

// --- Cel native bindings ---
export {
  Cel,
  type CelNative,
  type MonitorInfo,
  type WindowInfo,
  type KnowledgeFact,
  type RunRecord,
  type StepRecord,
  type ObservationRecord,
  type ScoredKnowledgeRecord,
  type EvictionResult,
} from "./cel-bindings.js";

// --- Capability interfaces (the contract Cel satisfies) ---
export type {
  ContextProvider,
  InputController,
  Planner,
  KnowledgeStore,
  BrowserBridge,
  EventSource,
  CelComposite,
} from "./interfaces/index.js";

// --- Canonical action execution ---
export { executeAction } from "./action-executor.js";
export {
  executePlannedAction,
  verifyActionOutcome,
  toActionOutcome,
  type AdapterCapabilities,
  type KernelActionOutcome,
  type KernelExecutionInput,
  type KernelEvent,
  type KernelEventType,
  type VerificationResult,
} from "./runtime/index.js";
export {
  AdapterRegistry,
  type AdapterInstance,
  type AdapterManifest,
  type AdapterState,
  type AdapterPlatform,
} from "./runtime/adapter-registry.js";

// --- Cortex / perception ---
export {
  Cortex,
  isCortexActive,
  getActiveCortex,
  getCortexById,
  getActiveCortexIds,
  type CortexOptions,
} from "./cortex.js";
export {
  PerceptionSession,
  isPerceptionSessionActive,
  isCortexActive as isPerceptionActive,
} from "./perception-socket.js";
export {
  normalizeCortexModel,
  normalizeCortexAnomalies,
} from "./cortex-normalize.js";
export {
  deriveSemanticInsight,
  deriveSourceSummary,
  enrichMentalModel,
} from "./cortex-insight.js";

// --- Dedicated CDP browser ---
export {
  DEFAULT_CEL_CDP_PORT,
  chooseChromiumBrowser,
  cleanupBlankCdpTabs,
  ensureDedicatedCdpBrowser,
  getCelCdpProfileRoot,
  getCanonicalCdpState,
  getDedicatedCdpBrowserStatus,
  getPreferredCelCdpPort,
  isCelOwnedUserDataDir,
  discoverCanonicalCdpTargets,
  selectPreferredCdpTarget,
  type CanonicalCdpState,
  type CanonicalCdpTarget,
  type CdpTargetLike,
  type ChromiumCandidate,
  type DedicatedCdpBrowserStatus,
  type EnsureDedicatedCdpBrowserOptions,
  type EnsureDedicatedCdpBrowserResult,
} from "./cdp-browser.js";

// --- Context utilities (assembly, diffing, serialization, compression) ---
export {
  assembleContext,
  formatContextSummary,
  type AssembledContext,
  type Observation,
  type ScoredKnowledge,
  type StepResult,
  type ContextAssemblyConfig,
} from "./context-assembly.js";
export {
  diffContexts,
  isDiffSignificant,
  formatDiffForPrompt,
  type ContextDiff,
  type ChangedElement,
} from "./context-differ.js";
export {
  serializeContextForLLM,
  resolveIndex,
  type SerializedContext,
} from "./context-serializer.js";
export {
  compressContext,
  type CompressionOptions,
} from "./context-compressor.js";
export {
  compactHistory,
  formatHistoryForCompaction,
  type CompactionConfig,
  type CompactedHistory,
} from "./message-compaction.js";
export {
  RunTranscript,
  type TranscriptEntry,
  type TranscriptEntryType,
} from "./transcript.js";

// --- Runtime helpers ---
export {
  SensitiveDataMasker,
  type SensitiveDataConfig,
} from "./sensitive-data.js";
export {
  isSkeletonScreen,
  skeletonWaitMs,
  hasActiveSpinner,
} from "./skeleton-detector.js";
export { UrlShortener } from "./url-shortener.js";
export {
  chunkMarkdown,
  formatExtractionPrompt,
  type ExtractionConfig,
  type PaginatedExtractionResult,
} from "./paginated-extractor.js";

// --- Configuration, logging, device baseline ---
export {
  celConfig,
  discoverClaudeCodeOauthTokens,
  hasConfiguredLlmAuth,
  hydrateLlmEnvFromConfig,
  resolvePath,
  type CelConfig,
} from "./config.js";
export { log, createLogger, setLogLevel, getLogLevel } from "./logger.js";
export { getOrScanBaseline, resetBaseline, type DeviceBaseline } from "./device-baseline.js";

// --- All public types (re-exported for convenience) ---
export type {
  Workflow,
  WorkflowStep,
  WorkflowAction,
  WorkflowStatus,
  Priority,
  ScreenContext,
  ContextElement,
  Bounds,
  ElementState,
  NetworkEvent,
  BoundsRegion,
  ContextReference,
  FocusedContext,
  CelEvent,
  PageContent,
  TextBlock,
  DomElement,
  PlannedStep,
  PlannedAction,
  PlannerStepRecord,
  GoalMetrics,
  ContextTier,
  PerceptionConfig,
  PerceptionDiff,
  ActionEntry,
  Anomaly,
  PulseResult,
  FeedResult,
  PerceptionSummary,
  TemporalFlags,
  ElementStability,
  MentalModel,
  CheckpointEntry,
  FreshnessAssessment,
  FreshnessState,
  StalenessCause,
  DiffSummary,
  ActionOutcome,
  TaskPhase,
  SemanticInsight,
  SourceSummary,
} from "./types.js";

// --- Canonical CEL action/perception contracts ---
export type {
  AdapterFactRef,
  AnomalyRef,
  AttemptRecord,
  Blocker,
  CanonicalAction,
  CanonicalStep,
  CanonicalStepResult,
  CapabilityRef,
  CortexMemory,
  DoneVerdict,
  EventRef,
  EvidenceRef,
  FailureReport,
  GoalOutcome,
  KnowledgeRef,
  MemoryKind,
  MemoryRef,
  NewCortexMemory,
  NextMove,
  OmittedCounts,
  PerceptionFrame,
  PlanningBudget,
  PlanningElement,
  PlanningElementState,
  PlanningScreen,
  PlanningView,
  ReviewDecision,
  RunProgress,
  RuntimeCaps,
} from "./langgraph/canonical.js";
