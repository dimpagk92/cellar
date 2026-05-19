Open Numbers (it should already be running with a blank sheet active). Use the CEL tools to:

1. Run `/cellar/healthcheck target_app:"Numbers"` or perform the equivalent read-only checks:
   - `cel_see` mode `windows`
   - `cel_see` mode `monitors`
   - `cel_see` mode `context`, filter `{ "detail": "summary" }`
   - `cel_see` mode `cdp_status` (CDP can be `not_needed` for this task)
2. Call `cel_see` with mode `windows` to confirm Numbers is visible and ready.
3. Call `cel_act` with action `write_cells`, app `Numbers`, and writes:
   - `A1` → `BTC`
   - `B1` → `ETH`
   - `C1` → `SOL`
   Set `verify: true`.
4. Call `cel_act` with action `read_cells`, app `Numbers`, cell_refs `["A1", "B1", "C1"]` to confirm the values stuck.

Report:

- healthcheck readiness
- write receipt id and dispatch path
- readback values
- whether verification passed
