from __future__ import annotations

import os
import subprocess
from pathlib import Path
from typing import Any

import pytest

from tests.common.helpers import (
    WebSession, find_free_port, http_request, initialize_server, start_stravia_server,
    stop_stravia_server, wait_for_setup_token, wait_until_ready,
)
from tests.e2e.storage.conftest import REPO_ROOT, storage_harness_binary
from tests.e2e.storage.test_usage_statistics import clear, stats

pytestmark = [pytest.mark.e2e, pytest.mark.storage]

WINDOW = {"start_at": 900000, "end_at": 3600000}
VIEWS = ("overview", "series", "models", "providers", "api-keys")


def overview(requests=0, errors=0, input_tokens=None, output_tokens=None,
             cache_read=None, duration=None, tps=None):
    return {
        "total_requests": requests, "error_count": errors,
        "total_input_tokens": input_tokens, "total_output_tokens": output_tokens,
        "total_cache_read_tokens": cache_read, "total_cache_write_tokens": None,
        "total_reasoning_tokens": None, "avg_duration_ms": duration,
        "avg_first_token_ms": None, "avg_output_tps": tps,
    }


def series_bucket(start, **values):
    result = overview(**values)
    result["request_count"] = result.pop("total_requests")
    return {"bucket_start": start, "bucket_ms": 900000, **result}


def snapshot(env):
    return {view: stats(env, view, **WINDOW, **({"bucket": 900} if view == "series" else {}))
            for view in VIEWS}


def assert_retained_contract(env, actual):
    # One completed client request, two model turns, and one admission failure.
    # Known output zero is not erased by the other wholly unknown attempt.
    assert actual["overview"] == overview(2, 1, 8, 0, 2, 200)
    assert actual["series"] == [
        series_bucket(900000, requests=2, errors=1),
        series_bucket(1800000, input_tokens=8, output_tokens=0, cache_read=2, duration=200, tps=0),
        series_bucket(2700000, duration=200),
    ]
    assert actual["models"] == [{
        "model_id": "deleted-model", "deleted": True, "model": "Old Model",
        "request_count": 2, "total_input_tokens": 8, "total_output_tokens": 0,
        "total_reasoning_tokens": None, "avg_duration_ms": 200,
    }]
    assert actual["providers"] == [{
        "provider_id": "deleted-provider", "deleted": True, "provider": "Old Provider",
        "request_count": 1, "error_count": 0, "avg_duration_ms": 100,
        "avg_output_tps": None,
    }]
    assert actual["api-keys"] == [{
        "api_key_id": "deleted-key", "deleted": True, "api_key_name": "Old Key",
        "request_count": 2, "total_input_tokens": 8, "total_output_tokens": 0,
        "cache_read_tokens": 2, "cache_write_tokens": None, "reasoning_tokens": None,
        "last_used_at": 2700000,
    }]
    # Adjacent absolute windows isolate known zero from unknown usage. Request
    # counts stay in the earlier client-start bucket, not the turn-start buckets.
    assert stats(env, start_at=1800000, end_at=2700000) == overview(0, 0, 8, 0, 2, 200, 0)
    assert stats(env, start_at=2700000, end_at=3600000) == overview(duration=200)
    assert stats(env) == actual["overview"]


@pytest.mark.parametrize("backend", ["sqlite", "postgres"], ids=["sqlite", "postgres"])
@pytest.mark.parametrize("fixture", ["empty", "retained"], ids=["empty0017", "retained0017"])
def test_supported0017_real_server_upgrade_retains_usage_without_duplicate_backfill(
    stravia_binary: Path, storage_runtime: dict[str, Any], tmp_path: Path,
    backend: str, fixture: str,
):
    """The preparer installs genuine old migrations; only the actual host upgrades.

    Existing Core interrupted-transaction tests complement this HTTP cutover
    acceptance; this test does not claim to inject host migration interruption.
    """
    schema = None
    process = None
    logs = []
    database: dict[str, Any] = {"backend": "sqlite"}
    prepare_env = os.environ.copy()
    prepare_env["STRAVIA_STORAGE_USAGE_UPGRADE_FIXTURE"] = fixture
    prepare_env["STRAVIA_STORAGE_BACKEND"] = backend
    try:
        if backend == "postgres":
            pg_url = storage_runtime["pg_url"]
            if not pg_url:
                pytest.fail("PostgreSQL upgrade HTTP acceptance requires isolated DB_URL")
            schema = storage_runtime["make_isolated_schema"]("stravia_usage_upgrade_http")
            storage_runtime["run_schema_action"]("create", pg_url=pg_url, schema=schema)
            dsn = storage_runtime["postgres_dsn_for_schema"](pg_url, schema)
            database = {"backend": "postgres", "url": dsn}
            prepare_env["STRAVIA_STORAGE_PG_URL"] = dsn
        else:
            sqlite_path = tmp_path / "db" / "gateway.db"
            sqlite_path.parent.mkdir(parents=True)
            prepare_env["STRAVIA_STORAGE_SQLITE_PATH"] = str(sqlite_path)
        prepared = subprocess.run(
            [str(storage_harness_binary())], env=prepare_env, cwd=REPO_ROOT,
            text=True, encoding="utf-8", capture_output=True, check=False, timeout=120,
        )
        assert prepared.returncode == 0, (prepared.stdout, prepared.stderr)
        assert f"usage_upgrade_fixture={fixture}" in prepared.stdout

        base = f"http://127.0.0.1:{find_free_port()}"
        args = ["--data-dir", str(tmp_path), "--host", "127.0.0.1",
                "--port", base.rsplit(":", 1)[1], *storage_runtime["server_args"](backend)]
        process, logs = start_stravia_server(stravia_binary=stravia_binary, args=args)
        wait_until_ready(f"{base}/api/v1/auth/state")
        session = initialize_server(base, wait_for_setup_token(logs, process), database)
        env = {"admin": base, "auth": session.auth_headers()}
        before = snapshot(env)
        if fixture == "retained":
            assert_retained_contract(env, before)
        else:
            assert before["overview"] == overview()
            assert before["models"] == before["providers"] == before["api-keys"] == []
            assert before["series"] == []

        # All identities came from retained observations, not live configuration.
        for resource, identity, error in [
            ("models", "deleted-model", "Route not found: deleted-model"),
            ("providers", "deleted-provider", "provider not found: deleted-provider"),
        ]:
            status, body = http_request("GET", f"{base}/api/v1/{resource}/{identity}", headers=env["auth"])
            assert status == 200, body
            assert body == {"error": error}
        if fixture == "retained":
            status, body = http_request(
                "GET", f"{base}/api/v1/observations/interactions/historical", headers=env["auth"],
            )
            assert status == 200, body
        clear(env)
        status, body = http_request(
            "GET", f"{base}/api/v1/observations/interactions/historical", headers=env["auth"],
        )
        assert status == 404, body
        assert snapshot(env) == before
        stop_stravia_server(process, logs)
        process = None
        process, logs = start_stravia_server(stravia_binary=stravia_binary, args=args)
        wait_until_ready(f"{base}/api/v1/auth/state")
        session = WebSession(base)
        status, body = session.request("POST", "/api/v1/auth/login", {
            "username": "admin", "password": "correct horse battery staple",
        })
        assert status == 200, body
        env["auth"] = session.auth_headers()
        assert snapshot(env) == before
        clear(env)
        assert snapshot(env) == before
    finally:
        if process is not None:
            stop_stravia_server(process, logs)
        if schema is not None:
            storage_runtime["run_schema_action"]("drop", pg_url=storage_runtime["pg_url"], schema=schema)
