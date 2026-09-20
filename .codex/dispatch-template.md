# Agent assignment record

Copy to `target/agent-dispatches/<unique-assignment-id>.md` before dispatch.
Follow [the durable policy](../AGENTS.md#agent-model-budget--durable-policy).
Replace every placeholder. This template does not launch or reconfigure an agent.

- Assignment ID / chunk / batch: <...>
- Role and allocation rationale: <...>
- Requested model / reasoning effort: <exact identifiers>
- Dispatch kind: <new agent | follow-up>
- Original dispatch record (required for follow-up): <path or not applicable>
- Agent ID (fill after dispatch): <pending>
- Exact spawn arguments, including fork_turns (or linked original): <...>
- Observed runtime model / effort and metadata source: <unknown unless exposed>
- Baseline / worktree: <...>
- Owned paths / shared seams requiring integration: <...>
- Bounded task and fixed contracts: <...>
- Dependencies / excluded work: <...>
- Validation manifest and exact acceptance commands: <path or inline>
- Escalation: <not applicable, or failing case + correction attempts + reason>
- Handoff: <changed paths, evidence links, remaining obligations>
- Actual token usage / cost / metadata source: <unknown unless exposed>
- Lead compliance review: <pending | pass | violation | unknown; explanation>

A requested model is not proof of an observed serving model or measured cost.
Missing explicit dispatch evidence must not be filled from repository defaults.
