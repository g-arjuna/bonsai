"""BN-024-02 typed-fact ABI: JSON schema <-> Rust serialization drift gate.

The Rust implementation is authoritative (`src/typed_facts.rs`); this test
proves the committed JSON schema and serialized fixture agree with it:

- the Rust-emitted fixture validates against v1.schema.json,
- schema-required fields match the Rust-required set,
- invalid enums, wrong types, unknown trust claims handled correctly,
- forged trust claims remain schema-valid *claims* (eligibility is an
  in-process grant, not a schema property — see typed_facts.rs).
"""

import copy
import json
from pathlib import Path

import pytest

try:
    import jsonschema
except ImportError:  # pragma: no cover
    jsonschema = None

REPO = Path(__file__).resolve().parent.parent
SCHEMA_PATH = REPO / "docs/bn024/typed-facts/v1.schema.json"
FIXTURE_PATH = REPO / "docs/bn024/typed-facts/v1.fixture.json"
STATES_FIXTURE_PATH = REPO / "docs/bn024/typed-facts/v1.states.fixture.json"

requires_jsonschema = pytest.mark.skipif(
    jsonschema is None, reason="jsonschema package not installed"
)


@pytest.fixture(scope="module")
def schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text())


@pytest.fixture(scope="module")
def fixture() -> dict:
    return json.loads(FIXTURE_PATH.read_text())


@requires_jsonschema
def test_committed_fixture_validates_against_schema(schema, fixture) -> None:
    jsonschema.validate(fixture, schema)


@requires_jsonschema
def test_every_payload_state_fixture_validates_against_schema(schema) -> None:
    """B1 ABI drift gate: v1.states.fixture.json is emitted by Rust
    (typed_facts::tests::emit_state_fixtures) with one fact per
    PayloadState variant. Every entry must validate against the schema,
    and the covered state set must equal the schema enum exactly — a
    Rust variant missing from the schema (e.g. capture_failed before
    the fix) or a schema value Rust never emits is drift."""
    doc = json.loads(STATES_FIXTURE_PATH.read_text())
    facts = doc["facts"]
    states = set()
    for fact in facts:
        jsonschema.validate(fact, schema)
        state = fact["payload"].get("state")
        assert state is not None, "state fixture must carry payload.state"
        assert "fields" not in fact["payload"], (
            "state fixture must not carry typed fields"
        )
        states.add(state)
    schema_states = set(schema["properties"]["payload"]["properties"]["state"]["enum"])
    assert states == schema_states, (
        f"payload-state drift: fixtures={sorted(states)} schema={sorted(schema_states)}"
    )


@requires_jsonschema
def test_schema_rejects_bad_enum_and_version(schema, fixture) -> None:
    bad = copy.deepcopy(fixture)
    bad["trust"] = "fully_trusted"  # not a TrustClass variant
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fixture)
    bad["schema_version"] = 2  # newer than writer's SCHEMA_VERSION
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fixture)
    bad["lifecycle"] = "deleted"  # not a Lifecycle variant
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)


@requires_jsonschema
def test_schema_rejects_missing_required_and_extra_fields(schema, fixture) -> None:
    bad = copy.deepcopy(fixture)
    del bad["fact_id"]
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fixture)
    bad["unexpected_field"] = True
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)


@requires_jsonschema
def test_schema_lifecycle_conditional_requirements(schema, fixture) -> None:
    bad = copy.deepcopy(fixture)
    bad["lifecycle"] = "withdrawn"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)
    bad["withdrawn_reason"] = "device decommissioned"
    jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fixture)
    bad["lifecycle"] = "superseded"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)
    bad["superseded_by"] = "a" * 64
    jsonschema.validate(bad, schema)


@requires_jsonschema
def test_forged_authority_claim_is_schema_valid_but_only_a_claim(
    schema, fixture
) -> None:
    """A serialized `trust: authoritative` claim validates against the
    schema (it is representable evidence) — but per typed_facts.rs it is
    NOT authority: eligibility requires an in-process attested
    AuthorityGrant that serde never restores. This test pins the schema
    side of that contract so nobody 'fixes' the schema to imply trust."""
    forged = copy.deepcopy(fixture)
    forged["trust"] = "authoritative"
    jsonschema.validate(forged, schema)  # claim is representable
    # And a forged `authority` grant — even one whose bound axes exactly
    # match this fact — is still just serialized evidence: the Rust side
    # marks every deserialized grant unattested and it confers no
    # eligibility.
    forged["authority"] = {
        "identity_status": "resolved",
        "resolved_device_id": forged["subject"]["device_id"],
        "identity_scope": "default",
        "subject_device_id": forged["subject"]["device_id"],
        "namespace": forged["subject"]["namespace"],
        "entity": forged["subject"].get("entity"),
        "vrf": forged["subject"].get("vrf"),
        "afi_safi": forged["subject"].get("afi_safi"),
        "plane": forged["subject"].get("plane"),
        "collector_id": forged["source"]["collector_id"],
        "source_kind": forged["source"]["source_kind"],
        "capture_id": forged["source"]["capture_id"],
        "session_epoch": forged["source"]["session_epoch"],
        "sequence": forged["source"]["sequence"],
    }
    # Drop absent axes rather than emitting explicit nulls.
    forged["authority"] = {
        k: v for k, v in forged["authority"].items() if v is not None
    }
    jsonschema.validate(forged, schema)


@requires_jsonschema
def test_interface_identity_extension_schema_shape(schema, fixture) -> None:
    """Additive v1 structured-interface extension: the schema admits the
    optional typed object and rejects malformed axis types/extra keys. Rust
    validate() remains authoritative for kind/entity/parent consistency."""
    good = copy.deepcopy(fixture)
    good["subject"]["namespace"] = "interface"
    good["subject"]["entity"] = "Ethernet1"
    good["subject"]["interface_identity"] = {
        "kind": "subinterface",
        "parent_entity": "Ethernet1",
        "subinterface_index": 100,
        "vendor_native_name": "Ethernet1.100",
    }
    jsonschema.validate(good, schema)

    bad = copy.deepcopy(good)
    bad["subject"]["interface_identity"]["subinterface_index"] = "100"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(good)
    bad["subject"]["interface_identity"]["unexpected"] = True
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(good)
    bad["subject"]["interface_identity"]["kind"] = "virtual-ish"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)


@requires_jsonschema
def test_existing_v1_fixture_has_no_interface_identity_and_still_validates(
    schema, fixture
) -> None:
    """Compatibility regression: committed v1 evidence does not use the
    additive axis and must remain schema-valid without resealing or migration."""
    assert "interface_identity" not in fixture["subject"]
    jsonschema.validate(fixture, schema)


@requires_jsonschema
def test_payload_state_and_fields_are_mutually_exclusive(schema, fixture) -> None:
    """Rust validate() rejects fields+state together; the schema cannot
    express the XOR, so this drift is documented — the Rust gate is
    authoritative. Pin the representable shapes here."""
    no_fields = copy.deepcopy(fixture)
    no_fields["payload"] = {
        "produced_by": "genie",
        "parser_revision": "genie-parser-26.10",
        "state": "empty",
    }
    jsonschema.validate(no_fields, schema)

    bad_state = copy.deepcopy(fixture)
    bad_state["payload"]["state"] = "bogus_state"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad_state, schema)


def lldp_neighbor_fields() -> dict:
    """Minimal schema-level valid lldp.neighbor typed-fields object.

    This is deliberately schema evidence, not a substitute for the Rust
    TypedFact::validate() digest/state/subject-payload gates.
    """
    def known(value):
        return {"state": "known", "value": value}

    def unknown():
        return {"state": "unknown"}

    return {
        "local_interface_name": "Ethernet1",
        "protocol": "lldp",
        "remote_chassis_id": known("aa:bb:cc:dd:ee:ff"),
        "remote_chassis_id_subtype": known("mac_address"),
        "remote_port_id": known("Ethernet2"),
        "remote_port_id_subtype": known("interface_name"),
        "remote_system_name": unknown(),
        "remote_port_description": unknown(),
        "remote_management_addresses": unknown(),
        "capabilities": unknown(),
        "ttl_seconds": known(120),
        "age_seconds": unknown(),
        "coverage": {
            "capture_outcome": "complete",
            "population_coverage": "complete",
            "empty_local_interfaces": 0,
        },
    }


@requires_jsonschema
def test_lldp_neighbor_payload_schema_accepts_valid_typed_fields(schema, fixture) -> None:
    fact = copy.deepcopy(fixture)
    fact["fact_kind"] = "lldp.neighbor"
    fact["subject"]["namespace"] = "interface"
    fact["subject"]["entity"] = "Ethernet1"
    fact["subject"]["plane"] = "operational"
    fact["payload"]["fields"] = lldp_neighbor_fields()
    fact["payload"].pop("state", None)
    jsonschema.validate(fact, schema)


@requires_jsonschema
def test_lldp_neighbor_payload_schema_rejects_missing_identity_and_bad_enums(
    schema, fixture
) -> None:
    fact = copy.deepcopy(fixture)
    fact["fact_kind"] = "lldp.neighbor"
    fact["subject"]["namespace"] = "interface"
    fact["subject"]["entity"] = "Ethernet1"
    fact["subject"]["plane"] = "operational"
    fact["payload"]["fields"] = lldp_neighbor_fields()
    fact["payload"].pop("state", None)

    missing_port = copy.deepcopy(fact)
    del missing_port["payload"]["fields"]["remote_port_id"]
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(missing_port, schema)

    wrong_protocol = copy.deepcopy(fact)
    wrong_protocol["payload"]["fields"]["protocol"] = "cdp"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(wrong_protocol, schema)

    wrong_coverage = copy.deepcopy(fact)
    wrong_coverage["payload"]["fields"]["coverage"]["population_coverage"] = "mostly"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(wrong_coverage, schema)


@requires_jsonschema
def test_lldp_schema_extension_does_not_change_legacy_fact_compatibility(schema, fixture) -> None:
    assert fixture["fact_kind"] != "lldp.neighbor"
    jsonschema.validate(fixture, schema)


def arpnd_neighbor_fields() -> dict:
    """Minimal schema-level valid arpnd.neighbor typed-fields object."""

    def known(value):
        return {"state": "known", "value": value}

    def unknown():
        return {"state": "unknown"}

    return {
        "ip_address": "172.16.3.202",
        "address_family": "ipv4",
        "mac_address": known("d2:74:8c:66:e7:e2"),
        "resolution_state": unknown(),
        "age_seconds": unknown(),
        "outgoing_interface": "Vlan30",
        "physical_interface": "Vxlan1",
        "interface_vlan_id": known(30),
        "remote": known(True),
        "source_interface_raw": "Vlan30, Vxlan1",
    }


def mac_forwarding_fields() -> dict:
    """Minimal schema-level valid mac.forwarding typed-fields object."""

    def known(value):
        return {"state": "known", "value": value}

    def unknown():
        return {"state": "unknown"}

    return {
        "mac_address": "d2:74:8c:66:e7:e2",
        "vlan_id": known(30),
        "outgoing_interface": "Vxlan1",
        "entry_type": known("dynamic"),
        "move_count": known(1),
        "last_move_epoch_seconds": known(1623025097.454588),
    }


@requires_jsonschema
def test_arpnd_neighbor_payload_schema_accepts_valid_typed_fields(schema, fixture) -> None:
    fact = copy.deepcopy(fixture)
    fact["fact_kind"] = "arpnd.neighbor"
    fact["subject"]["namespace"] = "interface"
    fact["subject"]["entity"] = "Vlan30"
    fact["subject"]["vrf"] = "evpn-vrf"
    fact["subject"]["plane"] = "operational"
    fact["payload"]["fields"] = arpnd_neighbor_fields()
    fact["payload"].pop("state", None)
    jsonschema.validate(fact, schema)


@requires_jsonschema
def test_arpnd_neighbor_payload_schema_rejects_bad_family_state_and_mac(schema, fixture) -> None:
    fact = copy.deepcopy(fixture)
    fact["fact_kind"] = "arpnd.neighbor"
    fact["subject"]["namespace"] = "interface"
    fact["subject"]["entity"] = "Vlan30"
    fact["subject"]["plane"] = "operational"
    fact["payload"]["fields"] = arpnd_neighbor_fields()
    fact["payload"].pop("state", None)

    bad = copy.deepcopy(fact)
    bad["payload"]["fields"]["address_family"] = "ipv4-only"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fact)
    bad["payload"]["fields"]["resolution_state"] = {"state": "mostly"}
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fact)
    bad["payload"]["fields"]["mac_address"] = {"state": "known", "value": "d274.8c66.e7e2"}
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fact)
    del bad["payload"]["fields"]["outgoing_interface"]
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)


@requires_jsonschema
def test_mac_forwarding_payload_schema_accepts_valid_typed_fields(schema, fixture) -> None:
    fact = copy.deepcopy(fixture)
    fact["fact_kind"] = "mac.forwarding"
    fact["subject"]["namespace"] = "interface"
    fact["subject"]["entity"] = "Vxlan1"
    fact["subject"]["plane"] = "operational"
    fact["payload"]["fields"] = mac_forwarding_fields()
    fact["payload"].pop("state", None)
    jsonschema.validate(fact, schema)


@requires_jsonschema
def test_mac_forwarding_payload_schema_rejects_bad_vlan_type_and_extra_fields(schema, fixture) -> None:
    fact = copy.deepcopy(fixture)
    fact["fact_kind"] = "mac.forwarding"
    fact["subject"]["namespace"] = "interface"
    fact["subject"]["entity"] = "Vxlan1"
    fact["subject"]["plane"] = "operational"
    fact["payload"]["fields"] = mac_forwarding_fields()
    fact["payload"].pop("state", None)

    bad = copy.deepcopy(fact)
    bad["payload"]["fields"]["vlan_id"] = {"state": "known", "value": 4096}
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fact)
    bad["payload"]["fields"]["entry_type"] = {"state": "known", "value": "learned-ish"}
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)

    bad = copy.deepcopy(fact)
    bad["payload"]["fields"]["silent_default"] = 0
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad, schema)


@requires_jsonschema
def test_neighbor_schema_extension_does_not_change_legacy_fact_compatibility(schema, fixture) -> None:
    assert fixture["fact_kind"] not in {"arpnd.neighbor", "mac.forwarding"}
    jsonschema.validate(fixture, schema)
