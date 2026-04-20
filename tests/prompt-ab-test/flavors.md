# Orchestrator Prompt A/B Test — Flavor Comparison

## Test Goals

| ID | Goal | Expected Tasks | Category |
|----|------|----------------|----------|
| simple-search | Open Chrome and search for 'weather today' | 2 | simple |
| form-fill | Go to the contact form, fill in name/email/message, submit | 3 | medium |
| file-management | Find report.pdf in Downloads, rename, move to Documents | 3 | medium |
| multi-app | Find data in spreadsheet, copy, open Slack, send to #finance | 4 | complex |
| research-task | Search pricing, find cost, paste into TextEdit | 4 | complex |
| ambiguous | Make the app look better | 1 | edge |
| single-action | Click the submit button | 1 | simple |
| conditional | If login showing, log in. Otherwise go to dashboard. | 2 | edge |

---

## Flavor A: Baseline (current production)

**Strategy:** Generic decomposition instructions.

```
You are a task decomposition agent. Break down a complex goal into sequential sub-tasks.
Each sub-task should be a self-contained goal that can be executed independently.
Keep sub-tasks specific and actionable.
Maximum {max} sub-tasks. Prefer fewer, broader tasks over many small ones.

For the FIRST task, also include a concrete first_action hint.
```

**Strengths:** Simple, general-purpose.
**Weaknesses:** No guidance on granularity. No screen awareness. May over-decompose.

---

## Flavor B: Strict Granularity

**Strategy:** Explicitly penalize over-splitting. Force screen-boundary awareness.

```
Your job is to break a goal into the MINIMUM number of sub-tasks needed.

RULES:
- Each sub-task must involve a DIFFERENT screen or application state
- Do NOT split actions within the same screen into separate tasks
- If a goal can be done on one screen, return exactly ONE task
- Filling a form = ONE task (not one per field)
- Navigation + action on destination = TWO tasks at most
- Maximum {max} sub-tasks. Aim for 1-3 in most cases.
```

**Strengths:** Prevents form-fill becoming 5 tasks. Fewer sub-tasks = fewer LLM calls.
**Weaknesses:** May under-decompose complex multi-app workflows.
**Hypothesis:** Best for simple/medium goals. May hurt complex goals.

---

## Flavor C: Action-Oriented

**Strategy:** Force imperative verbs and specific targets.

```
RULES:
- Start each task with an imperative verb: Click, Type, Navigate, Open, Find, Select, Copy, Paste
- Be SPECIFIC about targets: 'Click the Submit button' not 'Submit the form'
- Include exact data: 'Type john@test.com in the email field' not 'Fill in email'
- Group same-screen actions into one task
- Maximum {max} tasks. Keep under 4 when possible.
```

**Strengths:** Sub-agent gets very clear instructions. Less ambiguity = higher success rate.
**Weaknesses:** Longer descriptions = more tokens. May not adapt well to screen changes.
**Hypothesis:** Best for form-filling and specific interaction goals.

---

## Flavor D: Context-Aware

**Strategy:** Deeply use the current screen state to skip unnecessary steps.

```
Use the current screen context to make smart decisions.

RULES:
- If the current screen already shows what's needed for step 1, skip navigation
- If the app is already open, don't include 'Open the app' as a task
- Adapt the plan to what's CURRENTLY VISIBLE, not what you assume
- If the screen shows a form, make form-filling the first task
- If the screen shows search results, skip the search step
```

User prompt includes: "IMPORTANT: Plan based on what's currently visible."

**Strengths:** Avoids redundant "open app" tasks. Adapts to current state.
**Weaknesses:** Depends on context quality. If context is sparse, may produce bad plans.
**Hypothesis:** Best when context is rich (browser with CDP). Worst when context is sparse.

---

## Flavor E: Fallback-Aware

**Strategy:** Include Plan B hints in task descriptions.

```
Break goals into sub-tasks with fallback strategies.

RULES:
- Each task description should include the PRIMARY approach
- For tasks that might fail, add a fallback hint in parentheses:
  'Click Submit button (if not visible, press Enter)'
- Group same-screen actions into one task
```

**Strengths:** Sub-agent has recovery hints built in. Reduces need for replanning.
**Weaknesses:** Longer descriptions. Fallback hints may confuse the planner.
**Hypothesis:** Best for goals that commonly fail. May reduce replan frequency.

---

## Evaluation Criteria (scored 0-10)

| Criterion | What it measures | How scored |
|-----------|-----------------|------------|
| Task Count | Match to expected count | 10 - (3 × abs difference) |
| Specificity | Actionable descriptions | Has imperative verb? Has target? |
| Ordering | Valid dependency graph | Penalty for invalid dep references |
| First Action | Quality of first_action hint | Has imperative verb = 10 |
| Redundancy | No overlapping tasks | Penalty for >60% word overlap |

---

## Running the test

```bash
# Set your LLM provider
export CEL_LLM_PROVIDER=gemini
export CEL_LLM_MODEL=gemini-2.0-flash
export GEMINI_API_KEY=your-key

# Run
cd /path/to/cellar
npx tsx tests/prompt-ab-test/run-ab-test.ts
```

Results will show per-flavor scores, per-category winners, and the overall best prompt.
