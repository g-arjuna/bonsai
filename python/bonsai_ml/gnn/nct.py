"""Noise-Contrastive Training (NCT) pre-training for Bonsai GNN.

EV1-1 T7: Self-supervised pre-training using topology structure as supervision.

Intuition: Topologically adjacent devices (spine-leaf pairs, BGP peers) should
have similar embeddings. Randomly sampled non-adjacent pairs should not.

This addresses the label sparsity problem: even after 6 months, fault snapshots
may be <5% of total. NCT pre-trains the spatial GATv2 layers on structure before
supervised fine-tuning begins.

Phase 1 (this file): NCT pre-training on spatial layers only.
Phase 2 (train_anomaly.py): Supervised fine-tuning with fault labels + focal loss.

Gate: NCT runs only when snapshot_count >= 30 (configurable). Below this,
supervised fine-tuning starts from random init.

Deps: torch, torch-geometric (optional — module loads cleanly without them).
"""
from __future__ import annotations

import logging
import random
from dataclasses import dataclass
from pathlib import Path
from typing import Any

log = logging.getLogger(__name__)

MIN_SNAPSHOTS_FOR_NCT = 30
NCT_DEFAULT_TEMPERATURE = 0.07
NCT_DEFAULT_EPOCHS = 50
NCT_DEFAULT_LR = 1e-3
NCT_DEFAULT_NEGATIVE_SAMPLES = 16
NCT_GRAD_CLIP_NORM = 1.0
NCT_LR_WARMUP_STEPS = 100


@dataclass
class NctConfig:
    """Hyper-parameters for NCT pre-training."""
    temperature: float = NCT_DEFAULT_TEMPERATURE
    epochs: int = NCT_DEFAULT_EPOCHS
    lr: float = NCT_DEFAULT_LR
    negative_samples: int = NCT_DEFAULT_NEGATIVE_SAMPLES
    grad_clip_norm: float = NCT_GRAD_CLIP_NORM
    lr_warmup_steps: int = NCT_LR_WARMUP_STEPS
    min_snapshots: int = MIN_SNAPSHOTS_FOR_NCT
    checkpoint_path: str = "models/nct_pretrain.pt"


@dataclass
class NctTrainResult:
    """Outcome of a pre-training run."""
    final_loss: float
    best_loss: float
    epochs_completed: int
    checkpoint_path: str
    skipped: bool = False
    skip_reason: str = ""


class NodePairSampler:
    """Sample positive (adjacent) and negative (non-adjacent) node pairs.

    Positive pairs: topologically adjacent nodes from the same HeteroData
    snapshot — i.e. nodes connected by any edge type. These should have
    similar spatial embeddings.

    Negative pairs: randomly sampled node pairs that are NOT topologically
    adjacent. These should have dissimilar embeddings.

    The sampler operates on device nodes only (most anomaly-relevant type).
    """

    def __init__(self, negative_ratio: int = NCT_DEFAULT_NEGATIVE_SAMPLES) -> None:
        self.negative_ratio = negative_ratio

    def sample(self, hetero_data: Any) -> tuple[Any, Any, Any]:
        """Return (anchors, positives, negatives) index tensors for device nodes.

        Returns three 1D long tensors of equal length, where:
        - anchors[i] and positives[i] are adjacent device pairs (positive)
        - anchors[i] and negatives[i] are non-adjacent device pairs (negative)

        If torch is unavailable, raises RuntimeError immediately.
        """
        try:
            import torch
        except ImportError as exc:
            raise RuntimeError("NodePairSampler requires torch") from exc

        num_devices = hetero_data["device"].x.shape[0]
        if num_devices < 2:
            empty = torch.zeros(0, dtype=torch.long)
            return empty, empty, empty

        pos_edges: list[tuple[int, int]] = []

        for edge_type in hetero_data.edge_types:
            src_type, rel, dst_type = edge_type
            if src_type != "device" or dst_type != "device":
                continue
            try:
                ei = hetero_data[src_type, rel, dst_type].edge_index
            except (KeyError, AttributeError):
                continue
            if ei is None or ei.shape[1] == 0:
                continue
            ei_cpu = ei.cpu()
            for i in range(ei_cpu.shape[1]):
                src_idx = int(ei_cpu[0, i])
                dst_idx = int(ei_cpu[1, i])
                if src_idx != dst_idx:
                    pos_edges.append((src_idx, dst_idx))

        if not pos_edges:
            empty = torch.zeros(0, dtype=torch.long)
            return empty, empty, empty

        pos_set = set(pos_edges) | {(b, a) for a, b in pos_edges}

        anchors: list[int] = []
        positives: list[int] = []
        negatives: list[int] = []

        device_indices = list(range(num_devices))

        for anchor, positive in pos_edges:
            anchors.append(anchor)
            positives.append(positive)
            for _ in range(self.negative_ratio):
                candidate = random.choice(device_indices)
                attempts = 0
                while (candidate == anchor or (anchor, candidate) in pos_set) and attempts < 20:
                    candidate = random.choice(device_indices)
                    attempts += 1
                negatives.append(candidate)
                anchors.append(anchor)
                positives.append(positive)

        n = min(len(anchors), len(positives), len(negatives))
        anchors_t = torch.tensor(anchors[:n], dtype=torch.long)
        positives_t = torch.tensor(positives[:n], dtype=torch.long)
        negatives_t = torch.tensor(negatives[:n], dtype=torch.long)

        return anchors_t, positives_t, negatives_t


class NCTLoss:
    """Noise-contrastive loss for GNN pre-training.

    Loss = -log(exp(sim(z_i, z_j+) / τ) / Σ_k exp(sim(z_i, z_k-) / τ))

    where sim = cosine similarity, τ = temperature, j+ = topological neighbour,
    k- = random non-neighbour. The sum over negatives is an approximation of the
    full partition function.
    """

    def __init__(self, temperature: float = NCT_DEFAULT_TEMPERATURE) -> None:
        self.temperature = temperature

    def __call__(
        self,
        embeddings: Any,
        anchors: Any,
        positives: Any,
        negatives: Any,
    ) -> Any:
        """Compute NCT loss.

        Args:
            embeddings: (N, hidden_dim) device node embeddings.
            anchors: (M,) anchor node indices.
            positives: (M,) positive neighbour indices (same shape as anchors).
            negatives: (M,) negative (non-adjacent) node indices.

        Returns:
            Scalar loss tensor.
        """
        try:
            import torch
            import torch.nn.functional as F
        except ImportError as exc:
            raise RuntimeError("NCTLoss requires torch") from exc

        if anchors.shape[0] == 0:
            return torch.tensor(0.0, requires_grad=True)

        z_anchor = F.normalize(embeddings[anchors], dim=-1)
        z_pos = F.normalize(embeddings[positives], dim=-1)
        z_neg = F.normalize(embeddings[negatives], dim=-1)

        sim_pos = (z_anchor * z_pos).sum(dim=-1, keepdim=True) / self.temperature
        sim_neg = (z_anchor * z_neg).sum(dim=-1, keepdim=True) / self.temperature

        logits = torch.cat([sim_pos, sim_neg], dim=-1)
        labels = torch.zeros(logits.shape[0], dtype=torch.long, device=logits.device)
        return F.cross_entropy(logits, labels)


def pretrain_nct(
    model: Any,
    snapshots: list[Any],
    config: NctConfig | None = None,
    device_str: str = "cpu",
) -> NctTrainResult:
    """Pre-train the spatial GATv2 layers using NCT.

    Only runs if len(snapshots) >= config.min_snapshots. Below this gate,
    returns NctTrainResult(skipped=True) immediately.

    Args:
        model: HeteroGNN or STGNNModel (from model.py or stgnn.py).
            Only the spatial encoder layers are trained; temporal GRU layers
            (if present) are frozen during NCT.
        snapshots: List of HeteroData objects from the snapshot archive.
        config: NctConfig. Defaults to NctConfig().
        device_str: Torch device string ("cpu", "cuda", "mps").

    Returns:
        NctTrainResult with final loss and checkpoint path.
    """
    try:
        import torch
        import torch.optim as optim
    except ImportError as exc:
        raise RuntimeError("pretrain_nct requires torch") from exc

    if config is None:
        config = NctConfig()

    if len(snapshots) < config.min_snapshots:
        log.info(
            "NCT skipped: only %d snapshots available (need %d). "
            "Supervised fine-tuning will start from random init.",
            len(snapshots), config.min_snapshots,
        )
        return NctTrainResult(
            final_loss=0.0,
            best_loss=0.0,
            epochs_completed=0,
            checkpoint_path=config.checkpoint_path,
            skipped=True,
            skip_reason=f"insufficient_snapshots ({len(snapshots)} < {config.min_snapshots})",
        )

    device = torch.device(device_str)
    model = model.to(device)

    temporal_grus = getattr(model, "temporal_grus", None)
    if temporal_grus is not None:
        for param in temporal_grus.parameters():
            param.requires_grad = False
        log.info("NCT: frozen temporal GRU layers (pre-training spatial layers only)")

    optimizer = optim.AdamW(
        filter(lambda p: p.requires_grad, model.parameters()),
        lr=config.lr,
        weight_decay=1e-4,
    )

    def lr_lambda(step: int) -> float:
        if step < config.lr_warmup_steps:
            return float(step) / max(1, config.lr_warmup_steps)
        return 1.0

    scheduler = optim.lr_scheduler.LambdaLR(optimizer, lr_lambda)

    sampler = NodePairSampler(negative_ratio=config.negative_samples)
    nct_loss_fn = NCTLoss(temperature=config.temperature)

    best_loss = float("inf")
    step = 0

    for epoch in range(config.epochs):
        model.train()
        epoch_losses: list[float] = []

        snapshot_order = list(range(len(snapshots)))
        random.shuffle(snapshot_order)

        for snap_idx in snapshot_order:
            snapshot = snapshots[snap_idx]

            try:
                snapshot = snapshot.to(device)
            except (AttributeError, TypeError):
                pass

            anchors, positives, negatives = sampler.sample(snapshot)

            if anchors.shape[0] == 0:
                continue

            anchors = anchors.to(device)
            positives = positives.to(device)
            negatives = negatives.to(device)

            optimizer.zero_grad()

            if hasattr(model, "_encode_snapshot"):
                x_dict, _ = model._encode_snapshot(snapshot, return_attention=False)
                device_embeddings = x_dict.get("device")
            else:
                x_dict = {}
                for ntype, encoder in model.node_encoders.items():
                    if hasattr(snapshot[ntype], "x") and snapshot[ntype].x is not None:
                        x_dict[ntype] = encoder(snapshot[ntype].x)
                edge_index_dict = {}
                for edge_type in snapshot.edge_types:
                    try:
                        ei = snapshot[edge_type].edge_index
                        if ei is not None:
                            edge_index_dict[edge_type] = ei
                    except (KeyError, AttributeError):
                        pass
                for conv in model.convs:
                    x_dict = conv(x_dict, edge_index_dict)
                    import torch.nn.functional as F
                    x_dict = {k: F.relu(v) for k, v in x_dict.items()}
                device_embeddings = x_dict.get("device")

            if device_embeddings is None:
                continue

            loss = nct_loss_fn(device_embeddings, anchors, positives, negatives)
            if loss.requires_grad:
                loss.backward()
                torch.nn.utils.clip_grad_norm_(
                    model.parameters(), config.grad_clip_norm
                )
                optimizer.step()
                scheduler.step()
                step += 1

            epoch_losses.append(float(loss.detach()))

        avg_loss = sum(epoch_losses) / max(1, len(epoch_losses))
        if avg_loss < best_loss:
            best_loss = avg_loss

        if epoch % 10 == 0 or epoch == config.epochs - 1:
            log.info(
                "NCT pre-training epoch %d/%d — loss: %.4f (best: %.4f)",
                epoch + 1, config.epochs, avg_loss, best_loss,
            )

    if temporal_grus is not None:
        for param in temporal_grus.parameters():
            param.requires_grad = True
        log.info("NCT: re-enabled temporal GRU layers for supervised fine-tuning")

    checkpoint_path = Path(config.checkpoint_path)
    checkpoint_path.parent.mkdir(parents=True, exist_ok=True)
    torch.save(
        {
            "model_state_dict": model.state_dict(),
            "nct_config": config,
            "best_loss": best_loss,
            "num_snapshots": len(snapshots),
        },
        checkpoint_path,
    )
    log.info("NCT checkpoint saved to %s (best_loss=%.4f)", checkpoint_path, best_loss)

    return NctTrainResult(
        final_loss=avg_loss if epoch_losses else 0.0,
        best_loss=best_loss,
        epochs_completed=config.epochs,
        checkpoint_path=str(checkpoint_path),
    )
