"""Focal loss for Bonsai GNN training pipeline.

D5-T3 (DV1): Implements FocalLoss(gamma, alpha) from the TAGAE paper (CV6 T4-2
adoption). Default gamma=2, alpha=0.25 per TAGAE. Replaces cross-entropy in the
training script when training runs.

The implementation is a drop-in for ``torch.nn.CrossEntropyLoss`` in a
multi-class setting. It reduces to cross-entropy when gamma=0.

Dependency note: torch is OPTIONAL. Module loads without it; loss computation
raises RuntimeError with a clear message if torch is absent.
"""
from __future__ import annotations

from typing import Any


def focal_loss(
    inputs: Any,
    targets: Any,
    gamma: float = 2.0,
    alpha: float | list[float] | None = 0.25,
    reduction: str = "mean",
) -> Any:
    """Compute focal loss for multi-class classification.

    FL(p_t) = -alpha_t * (1 - p_t)^gamma * log(p_t)

    Args:
        inputs: Logits tensor of shape ``[N, C]``.
        targets: Ground-truth class indices of shape ``[N]``.
        gamma: Focusing parameter (default 2). gamma=0 is cross-entropy.
        alpha: Class-weight scalar or per-class list. None means no weighting.
        reduction: ``"mean"`` | ``"sum"`` | ``"none"``.

    Returns:
        Scalar loss (or per-sample tensor if reduction="none").
    """
    try:
        import torch
        import torch.nn.functional as F
    except ImportError as exc:
        raise RuntimeError(
            "focal_loss requires torch. Install with: pip install torch"
        ) from exc

    ce_loss = F.cross_entropy(inputs, targets, reduction="none")
    p_t = torch.exp(-ce_loss)
    focal_weight = (1.0 - p_t) ** gamma

    if alpha is not None:
        if isinstance(alpha, (int, float)):
            alpha_t = torch.full_like(ce_loss, alpha)
        else:
            alpha_tensor = torch.tensor(alpha, dtype=torch.float32, device=inputs.device)
            alpha_t = alpha_tensor[targets]
        focal_weight = alpha_t * focal_weight

    loss = focal_weight * ce_loss

    if reduction == "mean":
        return loss.mean()
    if reduction == "sum":
        return loss.sum()
    return loss


class FocalLoss:
    """Stateful wrapper around :func:`focal_loss` matching the nn.Module interface.

    Example usage in a training loop::

        criterion = FocalLoss(gamma=2.0, alpha=0.25)
        loss = criterion(logits, labels)
        loss.backward()
    """

    def __init__(
        self,
        gamma: float = 2.0,
        alpha: float | list[float] | None = 0.25,
        reduction: str = "mean",
    ) -> None:
        self.gamma = gamma
        self.alpha = alpha
        self.reduction = reduction

    def __call__(self, inputs: Any, targets: Any) -> Any:
        return focal_loss(
            inputs,
            targets,
            gamma=self.gamma,
            alpha=self.alpha,
            reduction=self.reduction,
        )

    def __repr__(self) -> str:
        return (
            f"FocalLoss(gamma={self.gamma}, alpha={self.alpha}, "
            f"reduction='{self.reduction}')"
        )
