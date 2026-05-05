"""Investigation agent loop (T3-1).

Implements the Anthropic tool-use ReAct pattern — functionally equivalent to
LangGraph's ReAct agent but with no additional dependency.  The loop:

  1. Send system prompt + detection context to the model.
  2. Model returns tool_use blocks → call tools, record in graph, feed results back.
  3. Repeat until model calls `summarise` (done) or budget exceeded.
  4. Write investigation result to core via POST /api/investigations/:id/complete.

Requires ANTHROPIC_API_KEY environment variable (never hardcoded).
"""
from __future__ import annotations

import json
import os
from typing import TYPE_CHECKING

from .budget import Budget, BudgetExceeded
from .tools import TOOL_SCHEMAS, call_tool

if TYPE_CHECKING:
    from bonsai_sdk.client import BonsaiClient

_MODEL = "claude-haiku-4-5-20251001"   # cost-efficient for tool-use loops
_MAX_TURNS = 12                         # hard cap on tool-call rounds

_SYSTEM = (
    "You are a network fault investigator for the bonsai network state engine. "
    "You have read-only access to the graph database through the provided tools. "
    "Investigate the reported detection by: (1) checking blast radius, "
    "(2) identifying affected applications, (3) reviewing recent detections and "
    "remediation history on the device. "
    "When you have enough information, call `summarise` with a plain-language summary. "
    "If a known playbook should be applied, also call `propose_playbook` before summarising. "
    "Never take autonomous action — proposals require operator approval."
)


def run_investigation(
    client: "BonsaiClient",
    investigation_id: str,
    detection_id: str,
    device_address: str,
    budget: Budget | None = None,
) -> dict:
    """Run the full investigation loop and return a result dict.

    Returns: {status, summary, proposal_json, tokens_used, cost_usd}
    On budget exceeded or model error, status="failed".
    """
    try:
        import anthropic  # type: ignore
    except ImportError:
        return {
            "status": "failed",
            "summary": "anthropic package not installed — pip install anthropic",
            "proposal_json": "",
            "tokens_used": 0,
            "cost_usd": 0.0,
        }

    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        return {
            "status": "failed",
            "summary": "ANTHROPIC_API_KEY not set",
            "proposal_json": "",
            "tokens_used": 0,
            "cost_usd": 0.0,
        }

    if budget is None:
        budget = Budget()

    anthropic_client = anthropic.Anthropic(api_key=api_key)
    messages: list[dict] = [
        {
            "role": "user",
            "content": (
                f"Investigate detection_id={detection_id} on device {device_address}. "
                "Use the available tools to gather context, then summarise your findings."
            ),
        }
    ]

    summary = ""
    proposal_json = ""
    status = "complete"

    try:
        for _ in range(_MAX_TURNS):
            resp = anthropic_client.messages.create(
                model=_MODEL,
                max_tokens=2048,
                system=_SYSTEM,
                tools=TOOL_SCHEMAS,
                messages=messages,
            )
            budget.charge(
                investigation_id,
                input_tokens=resp.usage.input_tokens,
                output_tokens=resp.usage.output_tokens,
            )
            budget.check(investigation_id)

            # Accumulate assistant message
            messages.append({"role": "assistant", "content": resp.content})

            tool_results = []
            done = False
            for block in resp.content:
                if block.type == "tool_use":
                    tool_output = call_tool(client, block.name, block.input)
                    output_str = json.dumps(tool_output)

                    # Record tool call in graph (fire-and-forget; errors non-fatal)
                    try:
                        client._http_json(
                            "POST",
                            f"/api/investigations/{investigation_id}/tool-calls",
                            {
                                "tool_name": block.name,
                                "input_json": json.dumps(block.input),
                                "output_json": output_str,
                            },
                        )
                    except Exception:
                        pass

                    tool_results.append({
                        "type": "tool_result",
                        "tool_use_id": block.id,
                        "content": output_str,
                    })

                    if block.name == "summarise":
                        summary = block.input.get("text", "")
                        done = True
                    elif block.name == "propose_playbook":
                        proposal_json = json.dumps(block.input)

            if tool_results:
                messages.append({"role": "user", "content": tool_results})

            if done or resp.stop_reason == "end_turn":
                break

    except BudgetExceeded as exc:
        status = "failed"
        summary = f"Budget exceeded: {exc}"
    except Exception as exc:
        status = "failed"
        summary = f"Agent error: {exc}"

    usage = budget.get_usage(investigation_id)
    tokens_used = usage.total_tokens if usage else 0
    cost_usd = usage.cost_usd if usage else 0.0

    return {
        "status": status,
        "summary": summary or "Investigation completed without explicit summary.",
        "proposal_json": proposal_json,
        "tokens_used": tokens_used,
        "cost_usd": cost_usd,
    }
