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

# Device feature indices that are structural (vendor/role OHE) and must NOT be perturbed.
# Indices 1-6: vendor OHE (6 vendors), 7-18: role OHE (12 roles).
# These are read-only identity features — perturbing them changes node identity.
STRUCTURAL_FEATURE_INDICES: tuple[int, ...] = tuple(range(1, 19))


@dataclass
class NoiseLevel:
    """Parameters for one noise curriculum phase."""
    edge_drop_prob: float = 0.0
    feature_perturb_prob: float = 0.0
    feature_perturb_scale: float = 0.2
    spurious_edge_prob: float = 0.0


@dataclass
class NoiseSchedule:
    """Three-phase noise curriculum for NCT pre-training.

    Phase 1 (epochs 1-10):  light  — drop 5% of edges.
    Phase 2 (epochs 11-30): medium — drop 15% edges + perturb 10% features ±0.2.
    Phase 3 (epoch 31+):    heavy  — drop 30% edges + perturb 20% features
                                     + add 5% spurious edges.

    Curriculum rationale: starting heavy makes the model resist structure
    entirely; the warm-up forces it to first learn coarse topology, then
    fine-grained operational features.
    """
    light:  NoiseLevel = None  # type: ignore[assignment]
    medium: NoiseLevel = None  # type: ignore[assignment]
    heavy:  NoiseLevel = None  # type: ignore[assignment]

    def __post_init__(self) -> None:
        if self.light is None:
            self.light  = NoiseLevel(edge_drop_prob=0.05)
        if self.medium is None:
            self.medium = NoiseLevel(edge_drop_prob=0.15, feature_perturb_prob=0.10)
        if self.heavy is None:
            self.heavy  = NoiseLevel(edge_drop_prob=0.30, feature_perturb_prob=0.20,
                                     spurious_edge_prob=0.05)

    def for_epoch(self, epoch: int) -> NoiseLevel:
        """Return the NoiseLevel applicable for the given 0-indexed epoch."""
        if epoch < 10:
            return self.light
        if epoch < 30:
            return self.medium
        return self.heavy


class NodeFeatureInvariance:
    """Applies feature perturbation while preserving structural (invariant) features.

    Structural features (vendor OHE, role OHE at indices 1-18) identify WHAT
    a device is — they must not be perturbed because perturbing them changes
    node identity, not operational state.

    Only operational features (cpu_util, memory, uptime, error rates, etc.)
    at indices outside the protected set are perturbed.

    Args:
        protected_indices: Feature column indices that must remain unmodified.
            Defaults to STRUCTURAL_FEATURE_INDICES (vendor+role OHE).
        perturb_scale: Gaussian noise std-dev applied to non-protected features.
    """

    def __init__(
        self,
        protected_indices: tuple[int, ...] = STRUCTURAL_FEATURE_INDICES,
        perturb_scale: float = 0.2,
    ) -> None:
        self.protected = set(protected_indices)
        self.perturb_scale = perturb_scale

    def perturb(self, x: Any, prob: float) -> Any:
        """Return a perturbed copy of node feature matrix x.

        Each non-protected feature column is independently perturbed with
        probability `prob`, adding Gaussian noise N(0, perturb_scale).
        Protected columns are copied unchanged.

        Args:
            x: (N, F) float tensor of node features.
            prob: Per-column perturbation probability in [0, 1].

        Returns:
            Perturbed copy of x (same shape, same dtype, same device).
        """
        if prob <= 0.0:
            return x
        try:
            import torch
        except ImportError:
            return x

        x_out = x.clone()
        num_features = x.shape[1]
        for col in range(num_features):
            if col in self.protected:
                continue
            if random.random() < prob:
                noise = torch.randn(x.shape[0], device=x.device) * self.perturb_scale
                x_out[:, col] = x_out[:, col] + noise
        return x_out


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
    noise_schedule: NoiseSchedule = None  # type: ignore[assignment]

    def __post_init__(self) -> None:
        if self.noise_schedule is None:
            self.noise_schedule = NoiseSchedule()


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

    EV1-8 T2 — Disconnected subgraph handling:
    Isolated nodes (degree = 0 across ALL device-device edge types) are
    EXCLUDED from positive pair sampling — no positive pair can be
    constructed from an isolated node. They remain valid negative samples
    (any non-neighbour is a valid negative for any anchor).
    """

    def __init__(self, negative_ratio: int = NCT_DEFAULT_NEGATIVE_SAMPLES) -> None:
        self.negative_ratio = negative_ratio

    def _compute_degrees(self, hetero_data: Any, num_devices: int) -> list[int]:
        """Return per-device degree (count of device-device edges, both directions)."""
        degrees = [0] * num_devices
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
                    if src_idx < num_devices:
                        degrees[src_idx] += 1
                    if dst_idx < num_devices:
                        degrees[dst_idx] += 1
        return degrees

    def sample(self, hetero_data: Any) -> tuple[Any, Any, Any]:
        """Return (anchors, positives, negatives) index tensors for device nodes.

        Returns three 1D long tensors of equal length, where:
        - anchors[i] and positives[i] are adjacent device pairs (positive)
        - anchors[i] and negatives[i] are non-adjacent device pairs (negative)

        Isolated nodes (degree=0) are excluded from positive pair sampling
        per EV1-8 T2. All nodes (including isolated) may appear as negatives.

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

        degrees = self._compute_degrees(hetero_data, num_devices)
        connected_indices = [i for i, d in enumerate(degrees) if d > 0]

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

        # All device indices are valid negatives (including isolated nodes).
        all_indices = list(range(num_devices))

        for anchor, positive in pos_edges:
            anchors.append(anchor)
            positives.append(positive)
            for _ in range(self.negative_ratio):
                candidate = random.choice(all_indices)
                attempts = 0
                while (candidate == anchor or (anchor, candidate) in pos_set) and attempts < 20:
                    candidate = random.choice(all_indices)
                    attempts += 1
                negatives.append(candidate)
                anchors.append(anchor)
                positives.append(positive)

        n = min(len(anchors), len(positives), len(negatives))
        anchors_t = torch.tensor(anchors[:n], dtype=torch.long)
        positives_t = torch.tensor(positives[:n], dtype=torch.long)
        negatives_t = torch.tensor(negatives[:n], dtype=torch.long)

        return anchors_t, positives_t, negatives_t

    def isolated_nodes(self, hetero_data: Any) -> list[int]:
        """Return indices of isolated device nodes (degree=0).

        These nodes apply mean-field approximation during NCT — they are
        embedded using only node features, skipping message passing.
        Exposed for external use by training pipelines that need to mask
        isolated nodes from the GRU temporal encoder.
        """
        try:
            num_devices = hetero_data["device"].x.shape[0]
        except (AttributeError, KeyError):
            return []
        degrees = self._compute_degrees(hetero_data, num_devices)
        return [i for i, d in enumerate(degrees) if d == 0]


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
    noise_schedule = config.noise_schedule
    feature_invariance = NodeFeatureInvariance()

    best_loss = float("inf")
    step = 0

    for epoch in range(config.epochs):
        model.train()
        epoch_losses: list[float] = []
        noise_level = noise_schedule.for_epoch(epoch)

        snapshot_order = list(range(len(snapshots)))
        random.shuffle(snapshot_order)

        for snap_idx in snapshot_order:
            snapshot = snapshots[snap_idx]

            try:
                snapshot = snapshot.to(device)
            except (AttributeError, TypeError):
                pass

            snapshot = _apply_noise(snapshot, noise_level, feature_invariance)
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


def _apply_noise(
    snapshot: Any,
    noise: NoiseLevel,
    feature_invariance: NodeFeatureInvariance,
) -> Any:
    """Return a noise-corrupted copy of a HeteroData snapshot.

    Applies three independent corruptions controlled by `noise`:
    1. Edge dropping  — randomly remove edges from all edge stores.
    2. Feature perturbation — add Gaussian noise to operational node features
       via NodeFeatureInvariance (structural features protected).
    3. Spurious edge insertion — add random edges to device-device relation.

    The original snapshot object is not modified.
    """
    try:
        import torch
        from torch_geometric.data import HeteroData
    except ImportError:
        return snapshot

    noisy = HeteroData()

    for node_type in snapshot.node_types:
        store = snapshot[node_type]
        if hasattr(store, "x") and store.x is not None:
            x = feature_invariance.perturb(store.x, noise.feature_perturb_prob)
            noisy[node_type].x = x
        if hasattr(store, "y") and store.y is not None:
            noisy[node_type].y = store.y
        if hasattr(store, "node_ids"):
            noisy[node_type].node_ids = store.node_ids

    for edge_type in snapshot.edge_types:
        src_type, rel, dst_type = edge_type
        try:
            ei = snapshot[src_type, rel, dst_type].edge_index
        except (KeyError, AttributeError):
            continue
        if ei is None or ei.shape[1] == 0:
            noisy[src_type, rel, dst_type].edge_index = ei
            continue

        if noise.edge_drop_prob > 0.0:
            mask = torch.rand(ei.shape[1]) > noise.edge_drop_prob
            ei = ei[:, mask]

        if noise.spurious_edge_prob > 0.0 and src_type == "device" and dst_type == "device":
            num_dev = snapshot["device"].x.shape[0] if hasattr(snapshot["device"], "x") else 0
            if num_dev >= 2:
                n_spurious = max(1, int(ei.shape[1] * noise.spurious_edge_prob))
                src_rand = torch.randint(0, num_dev, (n_spurious,))
                dst_rand = torch.randint(0, num_dev, (n_spurious,))
                spurious = torch.stack([src_rand, dst_rand], dim=0)
                ei = torch.cat([ei, spurious], dim=1)

        noisy[src_type, rel, dst_type].edge_index = ei

    return noisy
