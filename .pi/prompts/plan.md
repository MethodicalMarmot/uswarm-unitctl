---
description: Create a feature plan using the planning team (scout, architects, reviewer)
### How it works
#   1. User types /plan add WebSocket support
#   2. plan.md prompt creates the planning team → spawns all 5 agents in terminal panes
#   3. Team-lead sends the request to planner
#   4. planner orchestrates everything via team messaging:
#      - Messages scout → scout writes .pi/exchange/scout-findings.md
#      - Uses ask_user to gather requirements from the user
#      - Writes context files, messages rust-architect / frontend-architect
#      - Writes plan, messages plan-reviewer for review loop
#      - Notifies team-lead when done
#   5. Team-lead relays the result and shuts down the team
---

You are launching the **planning team** to create a feature plan.

The user's request: $@

## Instructions

1. **Create the team** from the `planning` predefined template:
   - Use `create_predefined_team` with `predefined_team: "planning"` and `team_name: "planning"` in the current working directory.

2. **Send the request to the planner**:
   - Use `send_message` to `planner` with the full user request quoted verbatim:
     > New planning request: $@

3. **Monitor progress**:
   - Periodically use `read_inbox` to check for messages from teammates.
   - Use `task_list` to monitor task progress.
   - When `planner` reports the plan is complete, relay the plan file path and summary to the user.

4. **Shutdown**:
   - Once the user acknowledges the plan, use `team_shutdown` to clean up.
