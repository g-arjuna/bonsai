use std::net::{Ipv4Addr, Ipv6Addr};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::config::TargetConfig;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PlatformProfile {
    pub platform_id: &'static str,
    pub aliases: &'static [&'static str],
    pub vendor: &'static str,
    pub os: &'static str,
    pub version_constraint: Option<&'static str>,
    pub supported_encodings: &'static [&'static str],
    pub supported_models: &'static [&'static str],
    pub bmp_support: &'static str,
    pub bgp_ls_support: &'static str,
    pub pcep_support: &'static str,
    pub adapter_id: Option<&'static str>,
    pub cli_running_config_cmd: Option<&'static str>,
    pub prefers_cli_without_tls: bool,
    pub fixture_ids: &'static [&'static str],
    pub last_verified_lab: Option<&'static str>,
}

pub const PLATFORM_PROFILES: &[PlatformProfile] = &[
    PlatformProfile {
        platform_id: "nokia.srlinux",
        aliases: &[
            "nokia.srlinux",
            "nokia_srlinux",
            "nokia-srlinux",
            "nokia_srl",
            "nokia-srl",
            "srl",
            "srlinux",
            "nokia",
        ],
        vendor: "nokia",
        os: "srlinux",
        version_constraint: Some(">=23.10"),
        supported_encodings: &["JSON_IETF"],
        supported_models: &["urn:nokia.com:srlinux", "openconfig"],
        bmp_support: "native",
        bgp_ls_support: "native",
        pcep_support: "conditional",
        adapter_id: Some("nokia-srlinux"),
        cli_running_config_cmd: Some("info from running"),
        prefers_cli_without_tls: false,
        fixture_ids: &["srl-24.10"],
        last_verified_lab: None,
    },
    PlatformProfile {
        platform_id: "nokia.sros",
        aliases: &["nokia.sros", "nokia_sros", "nokia-sros", "sros"],
        vendor: "nokia",
        os: "sros",
        version_constraint: Some(">=22.10"),
        supported_encodings: &["JSON_IETF", "PROTO"],
        supported_models: &["nokia-conf", "nokia-state", "openconfig"],
        bmp_support: "native",
        bgp_ls_support: "native",
        pcep_support: "conditional",
        adapter_id: Some("nokia-sros"),
        cli_running_config_cmd: Some("admin display-config"),
        prefers_cli_without_tls: false,
        fixture_ids: &[],
        last_verified_lab: None,
    },
    PlatformProfile {
        platform_id: "cisco.iosxr",
        aliases: &[
            "cisco.iosxr",
            "cisco_iosxr",
            "cisco-iosxr",
            "cisco_xrd",
            "cisco-xrd",
            "iosxr",
            "xrd",
            "vxr",
        ],
        vendor: "cisco",
        os: "iosxr",
        version_constraint: Some(">=7.9"),
        supported_encodings: &["JSON_IETF", "PROTO"],
        supported_models: &["Cisco-IOS-XR", "openconfig"],
        bmp_support: "native",
        bgp_ls_support: "native",
        pcep_support: "conditional",
        adapter_id: Some("cisco-iosxr"),
        cli_running_config_cmd: Some("show running-config"),
        prefers_cli_without_tls: true,
        fixture_ids: &["xrd-7.10.2"],
        last_verified_lab: None,
    },
    PlatformProfile {
        platform_id: "cisco.iosxe",
        aliases: &["cisco.iosxe", "cisco_iosxe", "cisco-iosxe", "iosxe"],
        vendor: "cisco",
        os: "iosxe",
        version_constraint: Some(">=17.9"),
        supported_encodings: &["JSON_IETF"],
        supported_models: &["Cisco-IOS-XE", "openconfig"],
        bmp_support: "conditional",
        bgp_ls_support: "conditional",
        pcep_support: "conditional",
        adapter_id: Some("cisco-iosxe"),
        cli_running_config_cmd: Some("show running-config"),
        prefers_cli_without_tls: true,
        fixture_ids: &[],
        last_verified_lab: None,
    },
    PlatformProfile {
        platform_id: "cisco.nxos",
        aliases: &["cisco.nxos", "cisco_nxos", "cisco-nxos", "nxos"],
        vendor: "cisco",
        os: "nxos",
        version_constraint: Some(">=10.2"),
        supported_encodings: &["JSON_IETF", "PROTO"],
        supported_models: &["Cisco-NX-OS-device", "openconfig"],
        bmp_support: "native",
        bgp_ls_support: "conditional",
        pcep_support: "unsupported",
        adapter_id: Some("cisco-nxos"),
        cli_running_config_cmd: Some("show running-config"),
        prefers_cli_without_tls: true,
        fixture_ids: &[],
        last_verified_lab: None,
    },
    PlatformProfile {
        platform_id: "arista.eos",
        aliases: &[
            "arista.eos",
            "arista_eos",
            "arista-eos",
            "arista_ceos",
            "arista-ceos",
            "arista",
            "eos",
            "ceos",
        ],
        vendor: "arista",
        os: "eos",
        version_constraint: Some(">=4.30"),
        supported_encodings: &["JSON_IETF"],
        supported_models: &["openconfig", "arista-exp-eos"],
        bmp_support: "native",
        bgp_ls_support: "native",
        pcep_support: "unsupported",
        adapter_id: Some("arista-eos"),
        cli_running_config_cmd: Some("show running-config"),
        prefers_cli_without_tls: true,
        fixture_ids: &["ceos-4.32.0F"],
        last_verified_lab: None,
    },
    PlatformProfile {
        platform_id: "juniper.junos",
        aliases: &[
            "juniper.junos",
            "juniper_junos",
            "juniper-junos",
            "juniper_crpd",
            "juniper-crpd",
            "juniper",
            "junos",
            "crpd",
            "vjunos",
        ],
        vendor: "juniper",
        os: "junos",
        version_constraint: Some(">=22.4"),
        supported_encodings: &["JSON_IETF", "PROTO"],
        supported_models: &["junos", "openconfig"],
        bmp_support: "native",
        bgp_ls_support: "native",
        pcep_support: "conditional",
        adapter_id: Some("juniper-junos"),
        cli_running_config_cmd: Some("show configuration | display set"),
        prefers_cli_without_tls: true,
        fixture_ids: &[],
        last_verified_lab: None,
    },
    PlatformProfile {
        platform_id: "frr",
        aliases: &["frr", "frrouting", "holo"],
        vendor: "frr",
        os: "frr",
        version_constraint: Some(">=8.5"),
        supported_encodings: &["JSON_IETF"],
        supported_models: &["frr-routing"],
        bmp_support: "native",
        bgp_ls_support: "conditional",
        pcep_support: "unsupported",
        adapter_id: Some("frr"),
        cli_running_config_cmd: Some("show running-config"),
        prefers_cli_without_tls: true,
        fixture_ids: &["frr-9.1.0"],
        last_verified_lab: Some("2026-10-11"),
    },
];

pub const UNKNOWN_PLATFORM: PlatformProfile = PlatformProfile {
    platform_id: "unknown",
    aliases: &["unknown", ""],
    vendor: "unknown",
    os: "unknown",
    version_constraint: None,
    supported_encodings: &[],
    supported_models: &[],
    // Standards-based BMP speakers can still export RFC 7854 telemetry even when
    // the platform is unknown and no vendor CLI adapter exists.
    bmp_support: "conditional",
    bgp_ls_support: "conditional",
    pcep_support: "unsupported",
    adapter_id: None,
    cli_running_config_cmd: None,
    prefers_cli_without_tls: false,
    fixture_ids: &[],
    last_verified_lab: None,
};

/// Resolve a raw vendor/platform string or alias to its canonical `PlatformProfile`.
/// Unknown strings resolve to `UNKNOWN_PLATFORM` — never to a Cisco default.
pub fn lookup_platform(raw: &str) -> &'static PlatformProfile {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() || trimmed == "unknown" {
        return &UNKNOWN_PLATFORM;
    }
    for profile in PLATFORM_PROFILES {
        if profile.platform_id == trimmed || profile.aliases.contains(&trimmed.as_str()) {
            return profile;
        }
    }
    &UNKNOWN_PLATFORM
}

/// Normalize any vendor/platform string to a canonical `platform_id` (`"unknown"` if unrecognized).
pub fn normalize_platform_id(raw: &str) -> &'static str {
    lookup_platform(raw).platform_id
}

/// Resolve a `TargetConfig`'s platform strictly from explicit/probed vendor metadata.
/// Hostname prefixes never fabricate platform support when `target.vendor` is unset or unknown.
pub fn resolve_target_platform(target: &TargetConfig) -> &'static PlatformProfile {
    match target.vendor.as_deref() {
        Some(v) => lookup_platform(v),
        None => &UNKNOWN_PLATFORM,
    }
}

/// Advisory-only helper that suggests a canonical `platform_id` from a hostname prefix
/// during interactive onboarding. Must not be used to claim adapter or protocol support.
pub fn suggest_platform_from_hostname(hostname: Option<&str>) -> Option<&'static str> {
    let host = hostname?.trim().to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    if host.starts_with("xrd") || host.contains("iosxr") {
        Some("cisco.iosxr")
    } else if host.starts_with("srl") || host.contains("srlinux") {
        Some("nokia.srlinux")
    } else if host.starts_with("ceos") || host.contains("arista") {
        Some("arista.eos")
    } else if host.starts_with("crpd") || host.starts_with("vjunos") || host.contains("junos") {
        Some("juniper.junos")
    } else if host.starts_with("frr") || host.starts_with("holo") {
        Some("frr")
    } else {
        None
    }
}

/// All valid vendor/platform strings accepted by the API and UI schema (canonical IDs + aliases).
pub fn all_schema_vendor_enums() -> Vec<&'static str> {
    let mut values = Vec::new();
    for profile in PLATFORM_PROFILES {
        if !values.contains(&profile.platform_id) {
            values.push(profile.platform_id);
        }
        for alias in profile.aliases {
            if !alias.is_empty() && !values.contains(alias) {
                values.push(alias);
            }
        }
    }
    values.push("unknown");
    values
}

/// Parse the host portion of an endpoint (`IPv4`, `IPv4:port`, `[IPv6]`, `[IPv6]:port`,
/// unbracketed `IPv6`, `hostname`, or `hostname:port`).
pub fn extract_endpoint_host(address: &str) -> Result<String> {
    let raw = address.trim();
    if raw.is_empty() {
        bail!("target address is empty");
    }
    if let Some(rest) = raw.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| anyhow!("invalid bracketed IPv6 target address '{address}'"))?;
        let host = &rest[..end];
        if host.is_empty() {
            bail!("empty bracketed host in '{address}'");
        }
        return Ok(host.to_string());
    }
    // Unbracketed IPv6 literal has multiple colons; validate and return as-is.
    if raw.matches(':').count() > 1 {
        if raw.parse::<Ipv6Addr>().is_ok() {
            return Ok(raw.to_string());
        }
        bail!("invalid unbracketed IPv6 address '{address}'");
    }
    if let Some((host, _port)) = raw.rsplit_once(':') {
        if host.is_empty() {
            bail!("missing host before port in '{address}'");
        }
        return Ok(host.to_string());
    }
    let _ = raw.parse::<Ipv4Addr>();
    Ok(raw.to_string())
}
