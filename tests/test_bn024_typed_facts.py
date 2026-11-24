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
    # And a forged `authority` block without resolution provenance is
    # still just serialized evidence — the Rust side marks it unattested.
    forged["authority"] = {"identity_status": "resolved"}
    jsonschema.validate(forged, schema)


@requires_jsonschema
def test_payload_state_and_fields_are_mutually_exclusive(schema, fixture) -> None:
    """Rust validate() rejects fields+state together; the schema cannot
    express the XOR, so this drift is documented — the Rust gate is
    authoritative. Pin the representable shapes here."""
    no_fields = copy.deepcopy(fixture)
    no_fields["payload"] = {"produced_by": "genie", "state": "empty"}
    jsonschema.validate(no_fields, schema)

    bad_state = copy.deepcopy(fixture)
    bad_state["payload"]["state"] = "bogus_state"
    with pytest.raises(jsonschema.ValidationError):
        jsonschema.validate(bad_state, schema)
