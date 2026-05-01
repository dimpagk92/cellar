# LangGraph Integration Notes

Date: April 24, 2026

This document is about one possible agent integration.
It is not the repository's primary architecture statement.

Read [docs/adapters-cel-agents.md](./adapters-cel-agents.md) first.

## Purpose

LangGraph is useful as a client runtime for CEL because it can provide:

- tool calling
- loop control
- retries and branching
- checkpointing
- human-in-the-loop

That makes it a good way to exercise CEL without spending time inventing orchestration machinery too early.

## Correct Framing

The correct framing is:

- `LangGraph` is one agent runtime
- `CEL` is the platform boundary
- `Adapters` remain the app-specific truth layer

So this is not:

- "the repo is LangGraph-first"

It is:

- "LangGraph is one supported way to drive CEL"

## Ownership

### LangGraph owns

- planning
- tool use
- retries
- branching
- checkpoints
- approval policies
- stop conditions

### CEL owns

- fused context
- screenshots and runtime capabilities
- canonical action execution
- adapter dispatch
- execution results

### Adapters own

- app-specific structured truth
- app-specific deterministic actions

## Boundary

For a LangGraph integration, the preferred tool boundary remains small:

- `see()`
- `act(action)`

Additional tools are acceptable when they expose real platform value, but the LangGraph path should not redefine CEL around a planner-specific interface.

## Why Keep This Path

The LangGraph path is still useful because it helps us:

- validate CEL against an external agent runtime
- prove that CEL is not tied to one planner
- learn which agent-facing contracts are stable

## What This Path Should Not Do

- define the repo's identity
- own adapter routing
- force CEL to become planner-specific
- become the only supported eval target

## Current Practical Use

Today, the LangGraph path is best treated as:

- a real integration
- a useful smoke-test client
- a runtime-specific acceptance target where appropriate

It should live comfortably beside other agent integrations, not above them.
