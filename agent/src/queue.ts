import type { Workflow, WorkflowStatus, Priority } from "./types.js";

/** An entry in the workflow queue. */
export interface QueueEntry {
  id: string;
  workflow: Workflow;
  priority: Priority;
  status: WorkflowStatus;
  enqueuedAt: Date;
  startedAt?: Date;
  completedAt?: Date;
}

const PRIORITY_ORDER: Record<Priority, number> = {
  critical: 0,
  high: 1,
  normal: 2,
  low: 3,
};

/**
 * Workflow queue — single active workflow at a time.
 * Additional workflows wait in a priority-ordered queue.
 */
export class WorkflowQueue {
  private active: QueueEntry | null = null;
  private queue: QueueEntry[] = [];
  private nextId = 1;

  /** Enqueue a workflow for execution. */
  enqueue(workflow: Workflow, priority: Priority = "normal"): string {
    const id = `wf-${this.nextId++}`;
    const entry: QueueEntry = {
      id,
      workflow,
      priority,
      status: "queued",
      enqueuedAt: new Date(),
    };
    this.queue.push(entry);
    this.queue.sort(
      (a, b) => PRIORITY_ORDER[a.priority] - PRIORITY_ORDER[b.priority]
    );
    return id;
  }

  /** Get the next workflow to run (dequeues it).
   *  Auto-releases stuck workflows after `staleTimeoutMs` (default: 5 minutes). */
  dequeue(staleTimeoutMs = 5 * 60 * 1000): QueueEntry | null {
    // Auto-release stuck workflows
    if (this.active?.startedAt) {
      const elapsed = Date.now() - this.active.startedAt.getTime();
      if (elapsed > staleTimeoutMs) {
        this.complete("failed");
      }
    }
    if (this.active) return null;
    const next = this.queue.shift();
    if (!next) return null;
    next.status = "running";
    next.startedAt = new Date();
    this.active = next;
    return next;
  }

  /** Mark the active workflow as completed. */
  complete(status: "completed" | "failed" = "completed"): void {
    if (this.active) {
      this.active.status = status;
      this.active.completedAt = new Date();
      this.active = null;
    }
  }

  /** Get the currently active workflow. */
  getActive(): QueueEntry | null {
    return this.active;
  }

  /** Get all queued workflows. */
  getQueue(): readonly QueueEntry[] {
    return this.queue;
  }

  /** Get queue length. */
  get length(): number {
    return this.queue.length;
  }
}
