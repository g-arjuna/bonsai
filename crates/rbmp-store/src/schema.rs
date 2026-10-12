/// DuckDB DDL for the rustybmp schema.
/// All tables use append-only inserts — no updates — to preserve history.

pub const CREATE_TABLES: &str = r#"
-- Route change events (announce / withdraw)
CREATE TABLE IF NOT EXISTS route_events (
    id              UUID        NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    speaker_addr    VARCHAR     NOT NULL,
    peer_addr       VARCHAR     NOT NULL,
    peer_as         UINTEGER    NOT NULL,
    rib_type        VARCHAR     NOT NULL,
    action          VARCHAR     NOT NULL,  -- 'announce' | 'withdraw'
    prefix          VARCHAR     NOT NULL,
    afi             VARCHAR     NOT NULL,
    origin          VARCHAR,
    as_path         VARCHAR,               -- space-separated ASN list
    as_path_len     USMALLINT,
    next_hop        VARCHAR,
    local_pref      UINTEGER,
    med             UINTEGER,
    communities     VARCHAR,               -- comma-separated
    ext_communities VARCHAR,
    large_communities VARCHAR,
    originator_id   VARCHAR,
    atomic_aggregate BOOLEAN DEFAULT false,
    only_to_customer UINTEGER,            -- RFC 9234 OTC ASN (NULL if absent)
    collector_id    VARCHAR               -- NULL for direct BMP connections (RV3-10)
);

-- BGP peer session events
CREATE TABLE IF NOT EXISTS peer_events (
    id              UUID        NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    speaker_addr    VARCHAR     NOT NULL,
    peer_addr       VARCHAR     NOT NULL,
    peer_as         UINTEGER,
    event_type      VARCHAR     NOT NULL,  -- 'peer_up' | 'peer_down'
    local_as        UINTEGER,
    hold_time       USMALLINT,
    capabilities    VARCHAR,               -- JSON array
    reason          VARCHAR,
    collector_id    VARCHAR
);

-- BMP speaker sessions
CREATE TABLE IF NOT EXISTS speaker_events (
    id              UUID        NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    speaker_addr    VARCHAR     NOT NULL,
    event_type      VARCHAR     NOT NULL,  -- 'speaker_up' | 'speaker_down'
    sys_name        VARCHAR,
    sys_descr       VARCHAR,
    reason          VARCHAR,
    collector_id    VARCHAR
);

-- Statistics snapshots (RFC 7854 + RFC 9972)
CREATE TABLE IF NOT EXISTS stats_events (
    id              UUID        NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    speaker_addr    VARCHAR     NOT NULL,
    peer_addr       VARCHAR     NOT NULL,
    counter_name    VARCHAR     NOT NULL,
    counter_value   UBIGINT     NOT NULL,
    stat_type       USMALLINT,          -- raw RFC type code
    afi             USMALLINT,          -- NULL for global stats
    safi            UTINYINT            -- NULL for global stats
);

-- EVPN route events (RFC 7432)
CREATE TABLE IF NOT EXISTS evpn_events (
    id              UUID        NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    speaker_addr    VARCHAR     NOT NULL,
    peer_addr       VARCHAR     NOT NULL,
    peer_as         UINTEGER    NOT NULL,
    action          VARCHAR     NOT NULL,   -- 'announce' | 'withdraw'
    route_type      UTINYINT    NOT NULL,   -- 1-5
    route_type_name VARCHAR     NOT NULL,
    rd              VARCHAR,               -- route distinguisher
    ethernet_tag    UINTEGER,
    mac             VARCHAR,               -- for type 2
    ip              VARCHAR,               -- for type 2/3/4/5
    prefix_len      UTINYINT,              -- for type 5
    mpls_label      UINTEGER,
    esi_hex         VARCHAR                -- 10-byte ESI as hex string
);

-- ML anomaly detections (written by Python pipeline, read by /api/ml/anomalies)
CREATE TABLE IF NOT EXISTS ml_anomalies (
    id          INTEGER,
    detected_at TIMESTAMPTZ NOT NULL,
    kind        VARCHAR     NOT NULL,   -- 'churn_zscore' | 'origin_change' | 'path_shortening' | 'flap'
    prefix      VARCHAR,               -- NULL for peer-level anomalies
    peer_addr   VARCHAR,               -- NULL for prefix-level anomalies
    score       DOUBLE,                -- anomaly score (z-score, IF score, flap count…)
    description VARCHAR,
    severity    VARCHAR                -- 'info' | 'warn' | 'critical'
);

-- SR Policy events (RV6-6, RFC 9252 / draft-ietf-idr-segment-routing-te-policy)
CREATE TABLE IF NOT EXISTS srpolicy_events (
    id              UUID        NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    speaker_addr    VARCHAR     NOT NULL,
    peer_addr       VARCHAR     NOT NULL,
    peer_as         UINTEGER    NOT NULL,
    action          VARCHAR     NOT NULL,   -- 'announce' | 'withdraw'
    endpoint        VARCHAR,               -- tunnel endpoint IP
    color           UINTEGER,              -- color extended community value
    preference      UINTEGER,             -- candidate path preference
    bsid            VARCHAR,               -- binding SID (MPLS label or SRv6 SID)
    segment_list    VARCHAR,               -- JSON array of segments
    distinguisher   UINTEGER              -- path distinguisher
);

-- ASPA validation results (RV6-6, RFC 9319)
CREATE TABLE IF NOT EXISTS aspa_validations (
    id              UUID        NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    prefix          VARCHAR     NOT NULL,
    peer_addr       VARCHAR     NOT NULL,
    peer_as         UINTEGER    NOT NULL,
    customer_asn    UINTEGER    NOT NULL,
    provider_asns   VARCHAR,               -- JSON array of provider ASNs in path
    result          VARCHAR     NOT NULL,  -- 'valid' | 'invalid' | 'unknown'
    direction       VARCHAR                -- 'upstream' | 'downstream'
);

-- RV7-P7: BGPsec ECDSA path validation verdicts (RFC 8205)
CREATE TABLE IF NOT EXISTS bgpsec_validations (
    occurred_at    TIMESTAMPTZ NOT NULL,
    prefix         VARCHAR     NOT NULL,
    peer_addr      VARCHAR     NOT NULL,
    as_path        VARCHAR,
    verdict        VARCHAR     NOT NULL,  -- 'valid' | 'invalid' | 'not_found' | 'malformed' | 'absent'
    invalid_hop    UTINYINT,              -- hop index where failure occurred (NULL when valid)
    invalid_reason VARCHAR               -- human-readable failure reason
);

CREATE INDEX IF NOT EXISTS idx_bgpsec_validations_prefix
ON bgpsec_validations (prefix, occurred_at DESC);

-- RV7-UI6: BGP convergence event detection (PeerDown → flood → EOR)
CREATE TABLE IF NOT EXISTS convergence_events (
    event_id           VARCHAR     NOT NULL PRIMARY KEY,
    started_at         TIMESTAMPTZ NOT NULL,
    eor_at             TIMESTAMPTZ,
    convergence_ms     DOUBLE,
    speaker_addr       VARCHAR     NOT NULL,
    peer_addr          VARCHAR     NOT NULL,
    trigger_type       VARCHAR,    -- 'peer_down' | 'mass_withdraw' | 'eor_timeout'
    affected_prefixes  UINTEGER,
    recovered_prefixes UINTEGER,
    unreachable_after  UINTEGER
);

CREATE INDEX IF NOT EXISTS idx_convergence_events_peer
ON convergence_events (peer_addr, started_at DESC);

-- RV7-B4: Fetched / inferred router policy configurations
CREATE TABLE IF NOT EXISTS policy_configs (
    fetched_at   TIMESTAMPTZ NOT NULL,
    peer_addr    VARCHAR     NOT NULL,
    speaker_addr VARCHAR     NOT NULL,
    policy_name  VARCHAR     NOT NULL,
    direction    VARCHAR     NOT NULL,  -- 'in' | 'out'
    vendor       VARCHAR     NOT NULL,
    clauses_json VARCHAR     NOT NULL,  -- serialized PolicyClause list (JSON)
    source       VARCHAR     NOT NULL,  -- 'ssh_genie' | 'ssh_paramiko' | 'pasted' | 'bmp_inferred'
    confidence   DOUBLE      NOT NULL
);

-- RV7-B4: Per-peer max-prefix limits (from PeerUp negotiation or operator config)
CREATE TABLE IF NOT EXISTS peer_max_prefix (
    updated_at   TIMESTAMPTZ NOT NULL,
    speaker_addr VARCHAR     NOT NULL,
    peer_addr    VARCHAR     NOT NULL,
    peer_as      UINTEGER    NOT NULL,
    afi_safi     VARCHAR     NOT NULL,  -- "ipv4-unicast" | "ipv6-unicast" etc.
    max_prefix   UINTEGER    NOT NULL,  -- configured limit
    warning_pct  USMALLINT   NOT NULL DEFAULT 75,
    PRIMARY KEY  (speaker_addr, peer_addr, afi_safi)
);

-- RV7-P3: Path Status TLV events (draft-ietf-grow-bmp-path-marking-tlv-05)
CREATE TABLE IF NOT EXISTS path_markings (
    occurred_at   TIMESTAMPTZ NOT NULL,
    speaker_addr  VARCHAR     NOT NULL,
    peer_addr     VARCHAR     NOT NULL,
    peer_as       UINTEGER    NOT NULL,
    prefix        VARCHAR     NOT NULL,
    afi           VARCHAR     NOT NULL,
    path_status   UINTEGER    NOT NULL,  -- 4-byte bitmap
    path_reason   USMALLINT   NOT NULL,  -- 2-byte reason code (0 = absent)
    status_label  VARCHAR     NOT NULL,  -- "best" | "backup" | "nonselected" | ...
    reason_label  VARCHAR     NOT NULL,  -- "not preferred: LOCAL_PREF" | ""
    collector_id  VARCHAR
);

-- Indexes for common query patterns
CREATE INDEX IF NOT EXISTS idx_route_events_prefix     ON route_events (prefix);
CREATE INDEX IF NOT EXISTS idx_route_events_peer       ON route_events (peer_addr);
CREATE INDEX IF NOT EXISTS idx_route_events_speaker    ON route_events (speaker_addr);
CREATE INDEX IF NOT EXISTS idx_route_events_time       ON route_events (occurred_at);
CREATE INDEX IF NOT EXISTS idx_route_events_as_path    ON route_events (as_path);
CREATE INDEX IF NOT EXISTS idx_peer_events_peer        ON peer_events (peer_addr);
CREATE INDEX IF NOT EXISTS idx_peer_events_time        ON peer_events (occurred_at);
CREATE INDEX IF NOT EXISTS idx_route_events_collector  ON route_events (collector_id);
CREATE INDEX IF NOT EXISTS idx_peer_events_collector   ON peer_events (collector_id);
-- RV6-6: Composite indexes for timeline and analytics queries
CREATE INDEX IF NOT EXISTS idx_route_events_prefix_time   ON route_events (prefix, occurred_at);
CREATE INDEX IF NOT EXISTS idx_route_events_peer_time     ON route_events (peer_addr, occurred_at);
CREATE INDEX IF NOT EXISTS idx_route_events_action_time   ON route_events (action, occurred_at);
CREATE INDEX IF NOT EXISTS idx_srpolicy_peer_time         ON srpolicy_events (peer_addr, occurred_at);
CREATE INDEX IF NOT EXISTS idx_aspa_prefix                ON aspa_validations (prefix);
CREATE INDEX IF NOT EXISTS idx_stats_peer_time            ON stats_events (peer_addr, occurred_at);
CREATE INDEX IF NOT EXISTS idx_path_markings_prefix_peer  ON path_markings (prefix, peer_addr, occurred_at);
CREATE INDEX IF NOT EXISTS idx_path_markings_status       ON path_markings (path_status, occurred_at);
"#;
