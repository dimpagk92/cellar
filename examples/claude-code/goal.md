Open Numbers (it should already be running with a blank sheet active). Use the CEL tools to:

1. Call `cel_see` with mode `windows` to confirm Numbers is the focused app.
2. Call `cel_act` with action `write_cells`, app `Numbers`, and writes:
   - `A1` → `BTC`
   - `B1` → `ETH`
   - `C1` → `SOL`
   Set `verify: true`.
3. Call `cel_act` with action `read_cells`, app `Numbers`, cell_refs `["A1", "B1", "C1"]` to confirm the values stuck.

Report what you wrote and what you read back.
