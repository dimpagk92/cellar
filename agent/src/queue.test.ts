import { describe, it, expect } from "vitest";
import { WorkflowQueue } from "./queue.js";
import type { Workflow } from "./types.js";

function makeWorkflow(name: string): Workflow {
  return {
    name,
    description: `Test workflow: ${name}`,
    app: "test-app",
    version: "1.0.0",
    steps: [
      {
        id: "step-1",
        description: "Click button",
        action: { type: "click", target: "btn" },
      },
    ],
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
  };
}

describe("WorkflowQueue", () => {
  it("should enqueue and dequeue workflows", () => {
    const queue = new WorkflowQueue();
    const wf = makeWorkflow("test-1");
    const id = queue.enqueue(wf, "normal");
    expect(id).toMatch(/^wf-/);
    expect(queue.length).toBe(1);

    const entry = queue.dequeue();
    expect(entry).not.toBeNull();
    expect(entry!.workflow.name).toBe("test-1");
    expect(entry!.status).toBe("running");
    expect(queue.length).toBe(0);
  });

  it("should return null when dequeuing empty queue", () => {
    const queue = new WorkflowQueue();
    expect(queue.dequeue()).toBeNull();
  });

  it("should block dequeue when active workflow exists", () => {
    const queue = new WorkflowQueue();
    queue.enqueue(makeWorkflow("wf-1"), "normal");
    queue.enqueue(makeWorkflow("wf-2"), "normal");

    queue.dequeue(); // Start wf-1
    const second = queue.dequeue(); // Should be blocked
    expect(second).toBeNull();
  });

  it("should allow dequeue after completing active workflow", () => {
    const queue = new WorkflowQueue();
    queue.enqueue(makeWorkflow("wf-1"), "normal");
    queue.enqueue(makeWorkflow("wf-2"), "normal");

    queue.dequeue(); // Start wf-1
    queue.complete("completed"); // Complete wf-1
    const second = queue.dequeue(); // Now wf-2 should work
    expect(second).not.toBeNull();
    expect(second!.workflow.name).toBe("wf-2");
  });

  it("should respect priority ordering", () => {
    const queue = new WorkflowQueue();
    queue.enqueue(makeWorkflow("low"), "low");
    queue.enqueue(makeWorkflow("critical"), "critical");
    queue.enqueue(makeWorkflow("normal"), "normal");
    queue.enqueue(makeWorkflow("high"), "high");

    const first = queue.dequeue();
    expect(first!.workflow.name).toBe("critical");
    queue.complete();

    const second = queue.dequeue();
    expect(second!.workflow.name).toBe("high");
    queue.complete();

    const third = queue.dequeue();
    expect(third!.workflow.name).toBe("normal");
    queue.complete();

    const fourth = queue.dequeue();
    expect(fourth!.workflow.name).toBe("low");
  });

  it("should track active workflow", () => {
    const queue = new WorkflowQueue();
    expect(queue.getActive()).toBeNull();

    queue.enqueue(makeWorkflow("active-test"), "normal");
    const entry = queue.dequeue();
    expect(queue.getActive()).toBe(entry);

    queue.complete();
    expect(queue.getActive()).toBeNull();
  });

  it("should return readonly queue", () => {
    const queue = new WorkflowQueue();
    queue.enqueue(makeWorkflow("q1"), "normal");
    queue.enqueue(makeWorkflow("q2"), "high");
    const items = queue.getQueue();
    expect(items.length).toBe(2);
    expect(items[0].workflow.name).toBe("q2"); // high priority first
  });

  it("should set startedAt and completedAt timestamps", () => {
    const queue = new WorkflowQueue();
    queue.enqueue(makeWorkflow("ts-test"), "normal");

    const entry = queue.dequeue()!;
    expect(entry.startedAt).toBeInstanceOf(Date);
    expect(entry.completedAt).toBeUndefined();

    queue.complete("completed");
    // After complete, the entry object was mutated
    expect(entry.completedAt).toBeInstanceOf(Date);
    expect(entry.status).toBe("completed");
  });

  it("should handle failed completion", () => {
    const queue = new WorkflowQueue();
    queue.enqueue(makeWorkflow("fail-test"), "normal");
    queue.dequeue();
    queue.complete("failed");
    expect(queue.getActive()).toBeNull();
  });

  it("should generate unique IDs", () => {
    const queue = new WorkflowQueue();
    const id1 = queue.enqueue(makeWorkflow("wf1"), "normal");
    const id2 = queue.enqueue(makeWorkflow("wf2"), "normal");
    const id3 = queue.enqueue(makeWorkflow("wf3"), "normal");
    expect(id1).not.toBe(id2);
    expect(id2).not.toBe(id3);
  });
});
