/// Path Status TLV parser — draft-ietf-grow-bmp-path-marking-tlv-05 §2
///
/// The TLV is embedded in BMP Route Monitoring messages.  It encodes the
/// router's BGP decision process outcome for each path: best, backup,
/// nonselected, filtered, stale, suppressed, etc., together with an optional
/// reason code explaining *why* a path was not selected.
///
/// Wire format (after the common 4-byte TLV header):
/// ```text
///  0                   1                   2                   3
///  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +---------------------------------------------------------------+
/// |                  Path Status bitmap (4 octets)                |
/// +-------------------------------+
/// |  Reason Code (2 oct, optional)|
/// +-------------------------------+
/// ```
use serde::{Deserialize, Serialize};

/// TLV type code used in BMP Route Monitoring TLV scanning.
///
/// draft-ietf-grow-bmp-path-marking-tlv-05 uses IANA TBD; Huawei VRP NE8000
/// (the primary implementation per the draft appendix) ships with type 6.
/// The value is configurable at the server layer; this constant is the default.
pub const PATH_STATUS_TLV_TYPE: u16 = 6;

/// Parsed Path Status TLV.
///
/// Multiple status bits may be set simultaneously, e.g. `Best | Primary`
/// or `Backup | AddPath`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PathStatusTlv {
    /// 4-byte status bitmap.  Zero means no status information.
    pub status: u32,
    /// Optional 2-byte reason code.  Zero means absent or not applicable.
    pub reason: u16,
}

// ─── Status bit constants ─────────────────────────────────────────────────────

pub const STATUS_INVALID: u32 = 0x0001;
pub const STATUS_BEST: u32 = 0x0002;
pub const STATUS_NONSELECTED: u32 = 0x0004;
pub const STATUS_PRIMARY: u32 = 0x0008;
pub const STATUS_BACKUP: u32 = 0x0010;
pub const STATUS_NON_INSTALLED: u32 = 0x0020;
pub const STATUS_BEST_EXTERNAL: u32 = 0x0040;
pub const STATUS_ADD_PATH: u32 = 0x0080;
pub const STATUS_FILTERED_INBOUND: u32 = 0x0100;
pub const STATUS_FILTERED_OUTBOUND: u32 = 0x0200;
pub const STATUS_STALE: u32 = 0x0400;
pub const STATUS_SUPPRESSED: u32 = 0x0800;

impl PathStatusTlv {
    // ── Status bit accessors ──────────────────────────────────────────────────

    pub fn is_invalid(&self) -> bool {
        self.status & STATUS_INVALID != 0
    }
    pub fn is_best(&self) -> bool {
        self.status & STATUS_BEST != 0
    }
    pub fn is_nonselected(&self) -> bool {
        self.status & STATUS_NONSELECTED != 0
    }
    pub fn is_primary(&self) -> bool {
        self.status & STATUS_PRIMARY != 0
    }
    pub fn is_backup(&self) -> bool {
        self.status & STATUS_BACKUP != 0
    }
    pub fn is_non_installed(&self) -> bool {
        self.status & STATUS_NON_INSTALLED != 0
    }
    pub fn is_best_external(&self) -> bool {
        self.status & STATUS_BEST_EXTERNAL != 0
    }
    pub fn is_add_path(&self) -> bool {
        self.status & STATUS_ADD_PATH != 0
    }
    pub fn is_filtered_inbound(&self) -> bool {
        self.status & STATUS_FILTERED_INBOUND != 0
    }
    pub fn is_filtered_outbound(&self) -> bool {
        self.status & STATUS_FILTERED_OUTBOUND != 0
    }
    pub fn is_stale(&self) -> bool {
        self.status & STATUS_STALE != 0
    }
    pub fn is_suppressed(&self) -> bool {
        self.status & STATUS_SUPPRESSED != 0
    }

    /// Human-readable label for the dominant (highest-priority) status bit.
    pub fn label(&self) -> &'static str {
        if self.is_best() {
            return "best";
        }
        if self.is_primary() {
            return "primary";
        }
        if self.is_backup() {
            return "backup";
        }
        if self.is_best_external() {
            return "best-external";
        }
        if self.is_add_path() {
            return "add-path";
        }
        if self.is_nonselected() {
            return "nonselected";
        }
        if self.is_filtered_inbound() {
            return "filtered-inbound";
        }
        if self.is_filtered_outbound() {
            return "filtered-outbound";
        }
        if self.is_stale() {
            return "stale";
        }
        if self.is_suppressed() {
            return "suppressed";
        }
        if self.is_non_installed() {
            return "non-installed";
        }
        if self.is_invalid() {
            return "invalid";
        }
        "unknown"
    }

    /// Human-readable reason for non-best selection (Table 2 from draft §2).
    pub fn reason_label(&self) -> &'static str {
        match self.reason {
            0x0001 => "AS loop",
            0x0002 => "unresolvable nexthop",
            0x0003 => "not preferred: LOCAL_PREF",
            0x0004 => "not preferred: AS_PATH length",
            0x0005 => "not preferred: ORIGIN type",
            0x0006 => "not preferred: MED",
            0x0007 => "not preferred: peer type",
            0x0008 => "not preferred: IGP cost",
            0x0009 => "not preferred: router-ID",
            0x000A => "not preferred: peer address",
            0x000B => "not preferred: AIGP",
            _ => "",
        }
    }

    /// Returns true if the path is actively contributing to forwarding
    /// (best or primary ECMP member).
    pub fn is_active(&self) -> bool {
        self.is_best() || self.is_primary()
    }
}

/// Parse a Path Status TLV from the raw TLV value bytes (the bytes that
/// follow the 4-byte common TLV type+length header).
///
/// Returns `None` if the data is too short to contain the 4-byte bitmap.
pub fn parse_path_status_tlv(data: &[u8]) -> Option<PathStatusTlv> {
    if data.len() < 4 {
        return None;
    }
    let status = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let reason = if data.len() >= 6 {
        u16::from_be_bytes([data[4], data[5]])
    } else {
        0
    };
    Some(PathStatusTlv { status, reason })
}

/// Explicit protocol profile governing experimental Path Status TLV decoding.
///
/// `draft-ietf-grow-bmp-path-marking-tlv-05` uses an IANA-TBD type code (commonly
/// type 6 in Huawei VRP lab captures) and is NOT part of RFC 9069 Loc-RIB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PathMarkingProfile {
    /// Disabled by default — do not scan arbitrary Route Monitoring bytes for draft TLVs.
    #[default]
    Disabled,
    /// Explicitly enabled experimental `draft-ietf-grow-bmp-path-marking-tlv-05` profile.
    ExperimentalDraft05 { tlv_type: u16 },
}

/// Scan a TLV-encoded byte slice under an explicit [`PathMarkingProfile`].
pub fn find_path_status_tlv_with_profile(
    data: &[u8],
    profile: PathMarkingProfile,
) -> Option<PathStatusTlv> {
    match profile {
        PathMarkingProfile::Disabled => None,
        PathMarkingProfile::ExperimentalDraft05 { tlv_type } => {
            find_path_status_tlv(data, tlv_type)
        }
    }
}

/// Scan a TLV-encoded byte slice for the Path Status TLV (type == `tlv_type`).
///
/// The slice is expected to start immediately after the BMP common peer header
/// (i.e., it is the enterprise/draft TLV extension trailer, never arbitrary BGP UPDATE bytes).
/// Returns the first matching TLV value, or `None`.
pub fn find_path_status_tlv(data: &[u8], tlv_type: u16) -> Option<PathStatusTlv> {
    // Guard against accidentally scanning a standard 16-byte BGP marker (`0xFF * 16`) as TLVs.
    if data.len() >= 16 && data[..16].iter().all(|&b| b == 0xFF) {
        return None;
    }
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let t = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let l = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + l > data.len() {
            break;
        }
        if t == tlv_type {
            return parse_path_status_tlv(&data[pos..pos + l]);
        }
        pos += l;
    }
    None
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_best_path() {
        // status=0x00000002 (Best), no reason code
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x02];
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_best());
        assert!(!tlv.is_backup());
        assert_eq!(tlv.label(), "best");
        assert_eq!(tlv.reason, 0);
        assert!(tlv.is_active());
    }

    #[test]
    fn test_parse_backup_with_reason() {
        // status=0x00000010 (Backup), reason=0x0006 (not preferred: MED)
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x10, 0x00, 0x06];
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_backup());
        assert!(!tlv.is_best());
        assert_eq!(tlv.label(), "backup");
        assert_eq!(tlv.reason_label(), "not preferred: MED");
        assert!(!tlv.is_active());
    }

    #[test]
    fn test_parse_filtered_inbound() {
        // status=0x00000100 (Filtered-inbound)
        let data: &[u8] = &[0x00, 0x00, 0x01, 0x00];
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_filtered_inbound());
        assert_eq!(tlv.label(), "filtered-inbound");
    }

    #[test]
    fn test_parse_multi_bit() {
        // Best (0x0002) | Primary (0x0008) = 0x000A
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x0A];
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_best());
        assert!(tlv.is_primary());
        assert!(tlv.is_active());
    }

    #[test]
    fn test_parse_too_short() {
        let data: &[u8] = &[0x00, 0x00, 0x00]; // only 3 bytes
        assert!(parse_path_status_tlv(data).is_none());
    }

    #[test]
    fn test_find_in_tlv_stream() {
        // TLV stream: [type=5, len=2, garbage], [type=6, len=4, status=0x0002], [type=7, len=0]
        let mut stream = vec![];
        stream.extend_from_slice(&[0x00, 0x05, 0x00, 0x02, 0xAA, 0xBB]); // type=5
        stream.extend_from_slice(&[0x00, 0x06, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02]); // type=6 (PATH_STATUS_TLV_TYPE)
        stream.extend_from_slice(&[0x00, 0x07, 0x00, 0x00]); // type=7, empty

        let tlv = find_path_status_tlv(&stream, PATH_STATUS_TLV_TYPE).unwrap();
        assert!(tlv.is_best());
        assert_eq!(tlv.reason, 0);
    }

    #[test]
    fn test_reason_labels() {
        let codes: &[(u16, &str)] = &[
            (0x0001, "AS loop"),
            (0x0003, "not preferred: LOCAL_PREF"),
            (0x0004, "not preferred: AS_PATH length"),
            (0x0008, "not preferred: IGP cost"),
        ];
        for &(code, expected) in codes {
            let tlv = PathStatusTlv {
                status: STATUS_NONSELECTED,
                reason: code,
            };
            assert_eq!(tlv.reason_label(), expected, "code {:#06x}", code);
        }
    }

    #[test]
    fn test_stale_bit() {
        let data: &[u8] = &[0x00, 0x00, 0x04, 0x00]; // STATUS_STALE = 0x0400
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_stale());
        assert_eq!(tlv.label(), "stale");
        assert!(!tlv.is_active());
    }

    #[test]
    fn test_suppressed_bit() {
        let data: &[u8] = &[0x00, 0x00, 0x08, 0x00]; // STATUS_SUPPRESSED = 0x0800
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_suppressed());
        assert_eq!(tlv.label(), "suppressed");
    }

    #[test]
    fn test_best_external_bit() {
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x40]; // STATUS_BEST_EXTERNAL = 0x0040
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_best_external());
        assert_eq!(tlv.label(), "best-external");
    }

    #[test]
    fn test_add_path_bit() {
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x80]; // STATUS_ADD_PATH = 0x0080
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_add_path());
        assert_eq!(tlv.label(), "add-path");
    }

    #[test]
    fn test_find_path_status_tlv_not_found() {
        let stream: &[u8] = &[0x00, 0x05, 0x00, 0x02, 0xAA, 0xBB]; // only type=5, no type=6
        let result = find_path_status_tlv(stream, PATH_STATUS_TLV_TYPE);
        assert!(
            result.is_none(),
            "Should return None when TLV type not present in stream"
        );
    }

    #[test]
    fn test_find_path_status_tlv_empty_stream() {
        let result = find_path_status_tlv(&[], PATH_STATUS_TLV_TYPE);
        assert!(result.is_none());
    }

    #[test]
    fn test_all_reason_codes_non_empty() {
        let all_codes = 0x0001u16..=0x000Bu16;
        for code in all_codes {
            let tlv = PathStatusTlv {
                status: STATUS_NONSELECTED,
                reason: code,
            };
            assert!(
                !tlv.reason_label().is_empty(),
                "reason_label for code {code:#06x} should not be empty"
            );
        }
    }

    #[test]
    fn test_unknown_reason_is_empty_string() {
        let tlv = PathStatusTlv {
            status: STATUS_NONSELECTED,
            reason: 0xFFFF,
        };
        assert_eq!(
            tlv.reason_label(),
            "",
            "unknown reason code should return empty string"
        );
    }

    #[test]
    fn test_nonselected_with_localpref_reason() {
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x04, 0x00, 0x03]; // NONSELECTED + reason=LOCAL_PREF
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_nonselected());
        assert_eq!(tlv.reason_label(), "not preferred: LOCAL_PREF");
        assert!(!tlv.is_active());
    }

    #[test]
    fn test_nonselected_with_aspath_reason() {
        let data: &[u8] = &[0x00, 0x00, 0x00, 0x04, 0x00, 0x04]; // NONSELECTED + reason=AS_PATH length
        let tlv = parse_path_status_tlv(data).unwrap();
        assert!(tlv.is_nonselected());
        assert_eq!(tlv.reason_label(), "not preferred: AS_PATH length");
    }

    #[test]
    fn test_default_tlv_is_zero() {
        let tlv = PathStatusTlv::default();
        assert_eq!(tlv.status, 0);
        assert_eq!(tlv.reason, 0);
        assert!(!tlv.is_best());
        assert!(!tlv.is_active());
        assert_eq!(tlv.label(), "unknown");
    }

    #[test]
    fn test_path_marking_profile_disabled_by_default_and_gated() {
        let stream: &[u8] = &[0x00, 0x06, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02];
        assert_eq!(PathMarkingProfile::default(), PathMarkingProfile::Disabled);
        assert!(
            find_path_status_tlv_with_profile(stream, PathMarkingProfile::default()).is_none(),
            "disabled profile must not scan or claim experimental path status TLVs"
        );
        let parsed = find_path_status_tlv_with_profile(
            stream,
            PathMarkingProfile::ExperimentalDraft05 {
                tlv_type: PATH_STATUS_TLV_TYPE,
            },
        )
        .expect("explicit experimental profile should decode TLV");
        assert!(parsed.is_best());
    }

    #[test]
    fn test_find_path_status_tlv_rejects_raw_bgp_pdu_stream() {
        let mut fake_bgp = vec![0xFFu8; 16];
        fake_bgp.extend_from_slice(&[0x00, 0x06, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02]);
        assert!(
            find_path_status_tlv(&fake_bgp, PATH_STATUS_TLV_TYPE).is_none(),
            "must not scan raw BGP messages starting with 16-byte 0xFF marker for coincidental TLVs"
        );
    }
}
