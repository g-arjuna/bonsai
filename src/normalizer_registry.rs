//! BN-024-03/03A — offline normalizer registry: the dispatch seam
//! between a *selected* `NosFeatureProfile` feature and a concrete
//! native normalizer.
//!
//! Scope: OFFLINE dispatch only. A caller holding command captures
//! (e.g. [`crate::interface_normalizer::InterfaceObservation`]) asks
//! the registry to normalize them under a selected profile+feature.
//! The registry verifies the feature is enabled, supported and bound
//! to a known normalizer id, checks the observation's declared NOS
//! matches the profile's platform, and invokes the normalizer. It
//! performs no collection, no I/O, no graph access and grants no
//! authority — output remains `device_asserted` evidence.
//!
//! Parser-chain integration is deliberately NOT wired here: capture
//! acquisition, transport and scheduling stay with their existing
//! owners. When BN-024-05+ lands a common capture envelope, dispatch
//! moves behind it without changing profile data.

use crate::interface_normalizer::{
    InterfaceObservation, NormalizationOutput, NormalizerNos, normalize,
};
use crate::nos_features::{FeatureStatus, ProfileRegistry, Selection};

/// Normalizer implementations known to this registry. Ids match the
/// `NormalizerRef.id` values profiles declare — profiles never name
/// code paths or executables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizerKind {
    /// `interface_normalizer::normalize` — the FRR
    /// `show int vrf all` + `ip -d address show` observation pair.
    InterfaceFrrCli,
}

/// One registered normalizer binding.
#[derive(Clone, Debug)]
struct NormalizerEntry {
    /// `NormalizerRef.id` this entry implements.
    id: &'static str,
    kind: NormalizerKind,
    /// Canonical platform families this normalizer is qualified for.
    /// A profile on any other platform cannot dispatch through it —
    /// e.g. standalone FRR does not inherit the cumulus/sonic
    /// qualification.
    qualified_families: &'static [&'static str],
}

/// Registry of offline normalizers. Immutable after construction;
/// adding a normalizer is a code change reviewed like any parser.
pub struct NormalizerRegistry {
    entries: Vec<NormalizerEntry>,
}

/// Why dispatch refused — always explicit, never a silent fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchError {
    /// Selection was not `Selected` (Candidate/Unsupported/Ambiguous/
    /// OutOfScope) — dispatch requires an executable selection.
    NotExecutableSelection,
    /// The feature is not `Enabled` on the selected profile.
    FeatureNotEnabled {
        feature: String,
        status: FeatureStatus,
    },
    /// The feature declares no normalizer binding.
    NoNormalizerBound { feature: String },
    /// The declared normalizer id is not registered.
    UnknownNormalizer { id: String },
    /// The normalizer is not qualified for this platform family.
    PlatformNotQualified { platform: String },
    /// The observation's declared NOS disagrees with the profile's
    /// platform — a cumulus normalizer path cannot be invoked under a
    /// sonic claim (or vice versa).
    ObservationPlatformMismatch {
        expected: NormalizerNos,
        got: NormalizerNos,
    },
}

/// Result of a successful dispatch: the normalization output plus the
/// profile's promotion state, so callers can label shadow evidence.
pub struct NormalizerRun {
    pub output: NormalizationOutput,
    /// `true` when the profile's feature promotion is `Shadow` — the
    /// output is comparison evidence only, never authoritative input.
    pub shadow_only: bool,
}

impl NormalizerRegistry {
    /// The built-in registry: currently only the PR #86 interface
    /// normalizer, qualified for the cumulus and sonic FRR
    /// observation path.
    pub fn builtin() -> Self {
        Self {
            entries: vec![NormalizerEntry {
                id: "interface_normalizer",
                kind: NormalizerKind::InterfaceFrrCli,
                qualified_families: &["sonic", "nvidia.cumulus"],
            }],
        }
    }

    /// Dispatch `obs` through the normalizer bound to `feature` on the
    /// profile inside `selection`. `feature` is the profile feature key
    /// (e.g. `"interfaces.frr_cli"`).
    pub fn dispatch(
        &self,
        selection: &Selection,
        feature: &str,
        obs: &InterfaceObservation,
    ) -> Result<NormalizerRun, DispatchError> {
        let Selection::Selected(profile) = selection else {
            return Err(DispatchError::NotExecutableSelection);
        };
        let flag =
            profile
                .features
                .get(feature)
                .ok_or_else(|| DispatchError::FeatureNotEnabled {
                    feature: feature.to_string(),
                    status: FeatureStatus::Absent,
                })?;
        // Device/feature-level applicability only: namespace-scoped
        // verification belongs to the structured-source slices and is
        // not implied here.
        let status = ProfileRegistry::feature_status(profile, feature);
        if status != FeatureStatus::Enabled {
            return Err(DispatchError::FeatureNotEnabled {
                feature: feature.to_string(),
                status,
            });
        }
        let nref = flag
            .normalizer
            .as_ref()
            .ok_or_else(|| DispatchError::NoNormalizerBound {
                feature: feature.to_string(),
            })?;
        let entry = self
            .entries
            .iter()
            .find(|e| e.id == nref.id)
            .ok_or_else(|| DispatchError::UnknownNormalizer {
                id: nref.id.clone(),
            })?;
        let family = platform_family(&profile.platform_family);
        if !entry
            .qualified_families
            .iter()
            .any(|f| *f == profile.platform_family || *f == family)
        {
            return Err(DispatchError::PlatformNotQualified {
                platform: profile.platform_family.clone(),
            });
        }
        // The observation must declare the NOS the profile selected —
        // never let a caller point the cumulus path at sonic input.
        let expected = nos_for_platform(&profile.platform_family).ok_or_else(|| {
            DispatchError::PlatformNotQualified {
                platform: profile.platform_family.clone(),
            }
        })?;
        if obs.nos != expected {
            return Err(DispatchError::ObservationPlatformMismatch {
                expected,
                got: obs.nos,
            });
        }
        let output = match entry.kind {
            NormalizerKind::InterfaceFrrCli => normalize(obs),
        };
        Ok(NormalizerRun {
            output,
            shadow_only: flag.promotion == crate::nos_features::PromotionState::Shadow,
        })
    }
}

/// Family portion of a canonical platform id — same rule as
/// `nos_features::platform_family` (kept local to avoid widening its
/// visibility contract).
fn platform_family(platform: &str) -> &str {
    platform.split(['.', '-']).next().unwrap_or(platform)
}

/// The `NormalizerNos` a platform's FRR CLI observation path uses.
/// Only the two qualified platforms map; anything else is refused.
fn nos_for_platform(platform_family_id: &str) -> Option<NormalizerNos> {
    match platform_family_id {
        "sonic" => Some(NormalizerNos::Sonic),
        "nvidia.cumulus" | "cumulus" => Some(NormalizerNos::Cumulus),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface_normalizer::{CaptureOutcome, CommandCapture};
    use crate::nos_features::{ProfileQuery, builtin_registry};

    fn obs(nos: NormalizerNos) -> InterfaceObservation<'static> {
        InterfaceObservation {
            nos,
            vtysh: CommandCapture::new("", CaptureOutcome::Complete),
            ip_addr: CommandCapture::new("", CaptureOutcome::Complete),
            context: crate::interface_normalizer::ObservationContext {
                device_id: "dev-1",
                scope: None,
                network_namespace: None,
                collector_id: "col-1",
                capture_id: "cap-1",
                session_epoch: 1,
                sequence: 0,
                observed_at: "2026-11-25T00:00:00Z",
                release_raw: None,
            },
        }
    }

    #[test]
    fn dispatch_runs_interface_normalizer_for_cumulus() {
        let reg = builtin_registry();
        let sel = reg.select(&ProfileQuery {
            platform: "nvidia.cumulus".into(),
            version: "5.13.0".into(),
            release: None,
            hardware: None,
        });
        let nr = NormalizerRegistry::builtin();
        let run = nr
            .dispatch(&sel, "interfaces.frr_cli", &obs(NormalizerNos::Cumulus))
            .expect("cumulus dispatch");
        assert!(run.shadow_only);
        // Empty captures -> explicit Empty status fact, still valid.
        assert_eq!(run.output.facts.len(), 1);
        assert!(run.output.facts.iter().all(|f| f.validate().is_ok()));
    }

    #[test]
    fn dispatch_refuses_non_selected_and_unqualified() {
        let reg = builtin_registry();
        let nr = NormalizerRegistry::builtin();
        // Standalone FRR profile selects but feature is disabled.
        let sel = reg.select(&ProfileQuery {
            platform: "frr".into(),
            version: "9.1".into(),
            release: None,
            hardware: None,
        });
        assert!(matches!(
            nr.dispatch(&sel, "interfaces.frr_cli", &obs(NormalizerNos::Cumulus)),
            Err(DispatchError::FeatureNotEnabled { .. })
        ));
        // SONiC profile with a cumulus observation -> mismatch.
        let sel = reg.select(&ProfileQuery {
            platform: "sonic".into(),
            version: "2025.1".into(),
            release: None,
            hardware: None,
        });
        assert!(matches!(
            nr.dispatch(&sel, "interfaces.frr_cli", &obs(NormalizerNos::Cumulus)),
            Err(DispatchError::ObservationPlatformMismatch { .. })
        ));
        // Unknown platform -> not executable.
        let sel = reg.select(&ProfileQuery {
            platform: "vyos".into(),
            version: "1".into(),
            release: None,
            hardware: None,
        });
        assert!(matches!(
            nr.dispatch(&sel, "interfaces.frr_cli", &obs(NormalizerNos::Cumulus)),
            Err(DispatchError::NotExecutableSelection)
        ));
    }
}
