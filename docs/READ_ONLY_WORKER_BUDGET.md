# Read-only worker turn budget

Read-only coordinator workers use a bounded twelve-turn ceiling. This preserves fail-closed execution while allowing live planner and risk-review tasks to complete when the provider requires more than six tool/model turns.

The live Minimax coding release gate is the production regression proof for this boundary.
