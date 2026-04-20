# CEL Test Scenarios — Safe Read-Only

All tests are **read-only** — no messages sent, no files modified, no external services written to.

## Tier 1: Simple (1-2 steps, routed)

| # | Goal | Expected Route | Expected Time |
|---|------|---------------|---------------|
| 1.1 | Open Terminal | open_app | <3s |
| 1.2 | Open Finder | open_app | <3s |
| 1.3 | Open System Settings | open_app | <3s |
| 1.4 | Take a screenshot | keyboard_sequence | <2s |
| 1.5 | Select all and copy | keyboard_sequence | <3s |

## Tier 2: App + Search (3-5 steps, routed)

| # | Goal | Expected Route | Expected Time |
|---|------|---------------|---------------|
| 2.1 | Open Finder and search for passport | open_and_search | <6s |
| 2.2 | Search the web for weather in Athens | search_web | <6s |
| 2.3 | Go to coinmarketcap.com | navigate_url | <6s |
| 2.4 | Open Chrome and search for latest Bitcoin price | search_web | <6s |

## Tier 3: Multi-step (5-10 steps, needs planner)

| # | Goal | Expected | Notes |
|---|------|----------|-------|
| 3.1 | Go to coinmarketcap.com and tell me the top 5 crypto prices | Read page content via CDP | READ ONLY — just report prices |
| 3.2 | Go to protothema.gr and read the top headlines | Read page content via CDP | READ ONLY — just report headlines |
| 3.3 | Check what time it is in the menu bar | Read a11y tree | READ ONLY |
| 3.4 | Read the title of every open Chrome tab | Read tab list | READ ONLY |

## Tier 4: Complex Workflows (10+ steps, multi-app)

These are aspirational — may not work yet. All READ ONLY.

| # | Goal | Steps | Notes |
|---|------|-------|-------|
| 4.1 | Go to coinmarketcap.com, collect the top 10 crypto prices, and summarize them in a note | Navigate → Read CDP → Summarize | READ ONLY — note is returned as text, not saved |
| 4.2 | Read the news from protothema.gr, understand the market sentiment, and draft a brief analysis | Navigate → Read CDP → Analyze | READ ONLY — analysis returned as text |
| 4.3 | Check my current Chrome tabs and tell me what I'm working on | Read window list → Read tab titles | READ ONLY |

## Safety Rules

- NEVER send messages (Slack, email, etc.)
- NEVER modify files (local or cloud)
- NEVER create/edit Google Sheets, Notion pages, or any external documents
- NEVER click "Send", "Submit", "Post", "Publish" or any destructive buttons
- ALL complex workflow outputs should be RETURNED AS TEXT in the goal result, not written anywhere
