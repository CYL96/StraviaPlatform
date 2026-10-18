"""Storage evidence exercises real SQLite rows and the production codec layout."""
import hashlib
import json
import sqlite3
from types import SimpleNamespace

import pytest
import zstandard

from tests.common.protocol_history_audit import _Reader, read_credential_evidence, read_history_evidence


def encode(raw, compressed=False):
    body = zstandard.ZstdCompressor(write_checksum=True).compress(raw) if compressed else raw
    return body + bytes([int(compressed)]) + b"\0" * 4 + len(raw).to_bytes(8, "little") + b"\1"


def digest(raw):
    number = int.from_bytes(hashlib.sha256(raw).digest(), "big")
    result = ""
    for _ in range(55):
        number, remainder = divmod(number, 26)
        result = chr(97 + remainder) + result
    return result


def raw(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode()


@pytest.fixture
def history(tmp_path):
    (tmp_path / "db").mkdir()
    connection = sqlite3.connect(tmp_path / "db" / "gateway.db")
    connection.executescript("""
        CREATE TABLE turn_chain_nodes(id TEXT PRIMARY KEY,parent_id TEXT,kind TEXT,principal TEXT,payload_version INTEGER,storage_format INTEGER,payload BLOB);
        CREATE TABLE turn_chain_contents(id INTEGER PRIMARY KEY,principal TEXT,content_key TEXT,content BLOB);
        CREATE TABLE turn_chain_node_contents(node_id TEXT,principal TEXT,content_id INTEGER);
    """)
    item = {"role": "assistant", "content": [{"type": "text", "text": "输出😀" * 100}]}
    item_raw = raw(item)
    key = digest(item_raw)
    state = dict.fromkeys(("context_fingerprint", "model", "provider_model", "selected_target_key",
                           "canonical_controls_fingerprint", "target_namespace", "protocol"), "")
    state["context_messages"] = 0
    payload = {"client_delta": {"messages": [], "system": None},
               "client_output": [None, None], "effective_system": None,
               "effective_output": {"id": "vendor", "model": "model", "items": [],
                                    "usage": {}, "vendor": {}},
               "trusted_media_turn_ids": [], "upstream_response_id": None,
               "effective_state": state, "effective_request": None}
    envelope = {"data": payload, "slots": [["/client_output/0", 0], ["/client_output/1", 0]], "contents": [key]}
    connection.execute("INSERT INTO turn_chain_nodes VALUES(?,?,?,?,?,?,?)", ("generation", None, "response", "private-owner", 6, 2, encode(raw(envelope), True)))
    connection.execute("INSERT INTO turn_chain_contents VALUES(?,?,?,?)", (1, "private-owner", key, encode(item_raw, True)))
    connection.execute("INSERT INTO turn_chain_node_contents VALUES(?,?,?)", ("generation", "private-owner", 1))
    connection.commit()
    yield tmp_path, connection, envelope, item
    connection.close()


def proof(history):
    directory, connection, _, _ = history
    connection.commit()
    return read_history_evidence(SimpleNamespace(database_url=None), directory, ["generation"])["generation"]


def test_restores_repeated_slots_and_hashes_real_output(history):
    result = proof(history)
    text = history[3]["content"][0]["text"] * 2
    assert result["read_verified"]
    assert result["payload_valid"] and result["references_valid"] and result["fingerprint_valid"]
    assert result["output_sha256"] == hashlib.sha256(text.encode()).hexdigest()
    assert result["content_count"] == 1 and result["slot_count"] == 2
    assert "private-owner" not in json.dumps(result)
    assert text not in json.dumps(result)


@pytest.mark.parametrize("mutation,code", [
    ("DELETE FROM turn_chain_contents", "reference_principal"),
    ("UPDATE turn_chain_contents SET content_key='wrong'", "content_fingerprint"),
    ("UPDATE turn_chain_node_contents SET principal='other'", "reference_principal"),
    ("UPDATE turn_chain_nodes SET parent_id=id", "parent_cycle"),
    ("UPDATE turn_chain_nodes SET parent_id='missing'", "node_missing"),
    ("UPDATE turn_chain_nodes SET storage_format=1", "payload_version"),
    ("UPDATE turn_chain_nodes SET kind='agent'", "node_identity"),
])
def test_rejects_corrupt_storage(history, mutation, code):
    history[1].execute(mutation)
    result = proof(history)
    assert not result["read_verified"]
    assert result["failures"] == [code]


def test_parent_principal_is_checked(history):
    connection = history[1]
    connection.execute("INSERT INTO turn_chain_nodes SELECT 'parent',NULL,kind,'another-owner',payload_version,storage_format,payload FROM turn_chain_nodes")
    connection.execute("UPDATE turn_chain_nodes SET parent_id='parent' WHERE id='generation'")
    assert proof(history)["failures"] == ["node_identity"]


def test_real_json_pointer_parent_before_child(history):
    directory, connection, envelope, item = history
    profile = {"output": [None], "a/b~c": "kept"}
    profile_raw = raw(profile)
    profile_key = digest(profile_raw)
    envelope["data"]["__open_responses_response_profile"] = None
    envelope["contents"].append(profile_key)
    envelope["slots"].extend([["/__open_responses_response_profile", 1], ["/__open_responses_response_profile/output/0", 0]])
    connection.execute("INSERT INTO turn_chain_contents VALUES(?,?,?,?)", (2, "private-owner", profile_key, encode(profile_raw)))
    connection.execute("INSERT INTO turn_chain_node_contents VALUES(?,?,?)", ("generation", "private-owner", 2))
    connection.execute("UPDATE turn_chain_nodes SET payload=?", (encode(raw(envelope)),))
    assert proof(history)["read_verified"]


def test_rejects_slot_out_of_range(history):
    envelope = history[2]
    envelope["slots"][0][1] = 3
    history[1].execute("UPDATE turn_chain_nodes SET payload=?", (encode(raw(envelope)),))
    assert proof(history)["failures"] == ["slot_index"]


def test_rejects_unknown_codec(history):
    history[1].execute("UPDATE turn_chain_contents SET content=?", (b"invalid" + b"\0" * 13 + b"\2",))
    assert proof(history)["failures"] == ["codec_version"]


def test_complete_ancestor_chain_is_restored(history):
    connection = history[1]
    connection.execute("INSERT INTO turn_chain_nodes SELECT 'parent',NULL,kind,principal,payload_version,storage_format,payload FROM turn_chain_nodes")
    connection.execute("INSERT INTO turn_chain_node_contents SELECT 'parent',principal,content_id FROM turn_chain_node_contents")
    connection.execute("UPDATE turn_chain_nodes SET parent_id='parent' WHERE id='generation'")
    result = proof(history)
    assert result["read_verified"] and result["ancestor_count"] == 1
    assert result["parent_id"] == "parent"
    assert result["content_count"] == 2


def test_decodes_escaped_pointer_tokens(history):
    envelope = history[2]
    envelope["data"]["a/b~c"] = None
    envelope["slots"].append(["/a~1b~0c", 0])
    history[1].execute("UPDATE turn_chain_nodes SET payload=?", (encode(raw(envelope)),))
    assert proof(history)["read_verified"]


def test_reference_rows_must_match_envelope(history):
    envelope = history[2]
    envelope["contents"] = []
    envelope["slots"] = []
    history[1].execute("UPDATE turn_chain_nodes SET payload=?", (encode(raw(envelope)),))
    assert proof(history)["failures"] == ["reference_set"]


def test_rejects_non_loopback_postgres_without_connecting(tmp_path):
    result = read_history_evidence(
        SimpleNamespace(database_url="postgresql://secret:token@192.0.2.1/db"),
        tmp_path, ["generation"],
    )["generation"]
    assert not result["read_verified"]
    assert result["failures"] == ["database_endpoint"]
    assert "secret" not in json.dumps(result) and "token" not in json.dumps(result)


@pytest.fixture
def credentials(tmp_path, monkeypatch):
    monkeypatch.setattr("tests.common.protocol_history_audit.time.time_ns",
                        lambda: 2_000_000_000_000_000_000)
    (tmp_path / "db").mkdir()
    connection = sqlite3.connect(tmp_path / "db" / "gateway.db")
    connection.executescript("""
        CREATE TABLE reversible_redaction_mappings(
            reference TEXT PRIMARY KEY NOT NULL, principal TEXT NOT NULL,
            secret TEXT NOT NULL, published_at INTEGER, created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
    """)
    secrets = ("ghp_" + "A" * 36, "ghp_" + "B" * 36)
    references = ("<!--sr:" + "a" * 28 + "-->", "<!--sr:" + "b" * 28 + "-->")
    for secret, reference in zip(secrets, references):
        connection.execute("INSERT INTO reversible_redaction_mappings VALUES(?,?,?,?,?,?,?)",
                           (reference, "private-owner", secret, 1_999_999_999_000,
                            1_999_999_999_000, 1_999_999_999_000, 2_000_000_001_000))
    connection.commit()
    expected = {"first": {"secrets": secrets, "references": references,
                          "new_references": references, "reused_references": 0},
                "second": {"secrets": secrets, "references": references,
                           "new_references": (), "reused_references": 2}}
    yield tmp_path, connection, expected
    connection.close()


def credential_proof(credentials):
    directory, connection, expected = credentials
    connection.commit()
    return read_credential_evidence(SimpleNamespace(database_url=None), directory, expected)


def test_reads_plaintext_mapping_publication_and_reused_identity(credentials):
    result = credential_proof(credentials)
    for nonce, new_count in (("first", 2), ("second", 0)):
        assert result[nonce] == {
            "read_verified": True,
            "principal_sha256": hashlib.sha256(b"private-owner").hexdigest(),
            "mapping_count": 2, "new_mapping_count": new_count,
            "published": True, "unexpired": True, "backend": "sqlite", "failures": [],
        }
    serialized = json.dumps(result)
    assert "private-owner" not in serialized
    for value in (*credentials[2]["first"]["secrets"],
                  *credentials[2]["first"]["references"]):
        assert value not in serialized
    assert credentials[1].execute("SELECT COUNT(*) FROM reversible_redaction_mappings").fetchone() == (2,)


@pytest.mark.parametrize("mutation,code", [
    ("DELETE FROM reversible_redaction_mappings", "credential_mapping_missing"),
    ("UPDATE reversible_redaction_mappings SET secret='wrong'", "credential_secret"),
    ("UPDATE reversible_redaction_mappings SET published_at=NULL", "credential_unpublished"),
    ("UPDATE reversible_redaction_mappings SET published_at=2000000000001", "credential_unpublished"),
    ("UPDATE reversible_redaction_mappings SET expires_at=2000000000000", "credential_expired"),
    ("UPDATE reversible_redaction_mappings SET principal='' ", "credential_principal"),
    ("UPDATE reversible_redaction_mappings SET principal=secret", "credential_principal"),
])
def test_credential_storage_rejects_real_row_mutations(credentials, mutation, code):
    credentials[1].execute(mutation)
    result = credential_proof(credentials)
    assert not result["first"]["read_verified"]
    assert result["first"]["failures"] == [code]


def test_credential_principal_must_be_shared_across_requests(credentials):
    expected = credentials[2]
    for index, nonce in enumerate(("first", "second")):
        expected[nonce] = {"secrets": (expected[nonce]["secrets"][index],),
                           "references": (expected[nonce]["references"][index],),
                           "new_references": (), "reused_references": 1}
    credentials[1].execute(
        "UPDATE reversible_redaction_mappings SET principal='other-owner' WHERE secret=?",
        (expected["second"]["secrets"][0],))
    result = credential_proof(credentials)
    assert result["first"]["read_verified"]
    assert result["second"]["failures"] == ["credential_principal"]


@pytest.mark.parametrize("change,code", [
    ({"references": ("<!--sr:" + "a" * 28 + "-->",) * 2}, "credential_distinct_mappings"),
    ({"reused_references": 1}, "credential_reuse_count"),
    ({"new_references": ("<!--sr:" + "z" * 28 + "-->",)}, "credential_reuse_count"),
    ({"secrets": ()}, "credential_expectations"),
])
def test_rejects_incomplete_or_inconsistent_credential_expectations(credentials, change, code):
    credentials[2]["first"].update(change)
    assert credential_proof(credentials)["first"]["failures"] == [code]


def test_rejects_changed_reference_for_reused_secret(credentials):
    expected = credentials[2]["second"]
    changed = "<!--sr:" + "c" * 28 + "-->"
    credentials[1].execute(
        "INSERT INTO reversible_redaction_mappings "
        "SELECT ?,principal,secret,published_at,created_at,updated_at,expires_at "
        "FROM reversible_redaction_mappings WHERE reference=?",
        (changed, expected["references"][0]))
    expected["references"] = (changed, expected["references"][1])
    assert credential_proof(credentials)["second"]["failures"] == ["credential_reused_identity"]


def test_duplicate_reference_rows_cannot_prove_mapping(credentials):
    credentials[1].executescript("""
        ALTER TABLE reversible_redaction_mappings RENAME TO original_mappings;
        CREATE TABLE reversible_redaction_mappings AS SELECT * FROM original_mappings;
        INSERT INTO reversible_redaction_mappings SELECT * FROM original_mappings;
    """)
    assert credential_proof(credentials)["first"]["failures"] == ["credential_mapping_missing"]


def test_credential_reader_uses_real_readonly_snapshot(credentials, monkeypatch):
    from tests.common import protocol_history_audit

    original_connect = protocol_history_audit._connect
    observed = []

    def connect(args, directory):
        connection, placeholder, backend = original_connect(args, directory)
        assert connection.in_transaction
        assert connection.execute("PRAGMA query_only").fetchone() == (1,)
        with pytest.raises(sqlite3.OperationalError):
            connection.execute("DELETE FROM reversible_redaction_mappings")
        observed.append(True)
        return connection, placeholder, backend

    monkeypatch.setattr(protocol_history_audit, "_connect", connect)
    assert credential_proof(credentials)["first"]["read_verified"]
    assert observed == [True]


def test_missing_mapping_table_returns_only_safe_failure(tmp_path):
    (tmp_path / "db").mkdir()
    sqlite3.connect(tmp_path / "db" / "gateway.db").close()
    expected = {"nonce": {"secrets": ("private-secret",),
                          "references": ("<!--sr:" + "a" * 28 + "-->",),
                          "new_references": (), "reused_references": 1}}
    result = read_credential_evidence(SimpleNamespace(database_url=None), tmp_path, expected)
    assert result["nonce"]["failures"] == ["credential_read_failed"]
    assert "private-secret" not in json.dumps(result)


def test_postgres_reader_adapts_bound_query_without_interpolating_secrets(credentials):
    # A real SQLite cursor verifies placeholder adaptation semantics; the
    # production PostgreSQL connection uses psycopg's %s rather than SQLite's ?.
    class PostgresParameters:
        def execute(self, query, values):
            assert query.count("%s") == 1 and "?" not in query
            assert values[0] not in query
            return credentials[1].execute(query.replace("%s", "?"), values)

    reader = _Reader(PostgresParameters(), "%s")
    reference = credentials[2]["first"]["references"][0]
    rows = reader.rows("SELECT secret FROM reversible_redaction_mappings WHERE reference=?",
                       (reference,))
    assert rows == [(credentials[2]["first"]["secrets"][0],)]
