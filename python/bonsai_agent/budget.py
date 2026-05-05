"""Per-investigation and daily token budget (T3-3).

Budget is fail-closed: if the per-investigation limit is exceeded, the agent
loop raises BudgetExceeded and records the investigation as failed.

Usage:
    budget = Budget(per_investigation=50_000, daily=500_000)
    budget.charge(investigation_id="...", input_tokens=120, output_tokens=80)
    budget.check(investigation_id="...")   # raises BudgetExceeded if over limit

Daily budget resets at UTC midnight on the next check after midnight.
"""
from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Optional

# Default limits (tokens)
DEFAULT_PER_INVESTIGATION = 50_000
DEFAULT_DAILY = 500_000

# Approximate Anthropic pricing (claude-3-5-haiku) — for cost estimation only.
# Update if model or pricing changes.
_INPUT_COST_PER_M  = 0.80   # USD per 1M input tokens
_OUTPUT_COST_PER_M = 4.00   # USD per 1M output tokens


class BudgetExceeded(Exception):
    pass


@dataclass
class InvestigationUsage:
    investigation_id: str
    input_tokens: int = 0
    output_tokens: int = 0

    @property
    def total_tokens(self) -> int:
        return self.input_tokens + self.output_tokens

    @property
    def cost_usd(self) -> float:
        return (
            self.input_tokens  / 1_000_000 * _INPUT_COST_PER_M
            + self.output_tokens / 1_000_000 * _OUTPUT_COST_PER_M
        )


@dataclass
class Budget:
    per_investigation: int = DEFAULT_PER_INVESTIGATION
    daily: int = DEFAULT_DAILY

    _usage: dict[str, InvestigationUsage] = field(default_factory=dict, repr=False)
    _daily_input: int = field(default=0, repr=False)
    _daily_output: int = field(default=0, repr=False)
    _day_epoch: int = field(default_factory=lambda: _today_epoch(), repr=False)

    # ── public interface ──────────────────────────────────────────────────────

    def charge(self, investigation_id: str, input_tokens: int, output_tokens: int) -> None:
        self._roll_day_if_needed()
        u = self._usage.setdefault(
            investigation_id, InvestigationUsage(investigation_id)
        )
        u.input_tokens  += input_tokens
        u.output_tokens += output_tokens
        self._daily_input  += input_tokens
        self._daily_output += output_tokens

    def check(self, investigation_id: str) -> None:
        """Raise BudgetExceeded if any limit is breached."""
        self._roll_day_if_needed()
        u = self._usage.get(investigation_id)
        if u and u.total_tokens > self.per_investigation:
            raise BudgetExceeded(
                f"investigation {investigation_id} used {u.total_tokens} tokens "
                f"(limit {self.per_investigation})"
            )
        daily_total = self._daily_input + self._daily_output
        if daily_total > self.daily:
            raise BudgetExceeded(
                f"daily budget exceeded: {daily_total} tokens used (limit {self.daily})"
            )

    def get_usage(self, investigation_id: str) -> Optional[InvestigationUsage]:
        return self._usage.get(investigation_id)

    def daily_summary(self) -> dict:
        return {
            "input_tokens": self._daily_input,
            "output_tokens": self._daily_output,
            "total_tokens": self._daily_input + self._daily_output,
            "daily_limit": self.daily,
            "investigations": len(self._usage),
            "cost_usd": (
                self._daily_input  / 1_000_000 * _INPUT_COST_PER_M
                + self._daily_output / 1_000_000 * _OUTPUT_COST_PER_M
            ),
        }

    # ── internals ─────────────────────────────────────────────────────────────

    def _roll_day_if_needed(self) -> None:
        today = _today_epoch()
        if today != self._day_epoch:
            self._day_epoch    = today
            self._daily_input  = 0
            self._daily_output = 0


def _today_epoch() -> int:
    """UTC day number (seconds since epoch // 86400)."""
    return int(time.time()) // 86_400
