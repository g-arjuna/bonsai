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
