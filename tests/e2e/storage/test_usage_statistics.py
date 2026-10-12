from __future__ import annotations

import json
import sqlite3
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing, contextmanager
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

import pytest

from tests.common.helpers import (
    find_free_port, http_request, initialize_server, start_stravia_server,
    stop_stravia_server, wait_for_setup_token, wait_until_ready,
)
from tests.e2e.admin.test_observations import (
    _create_route, _detail, _finished_detail, _proxy, _route_interactions, _wait_for,
)

pytestmark = [pytest.mark.e2e, pytest.mark.storage]


@contextmanager
def usage_upstream():
    """Only the external provider is scripted; gateway/storage remain real."""
    entered = threading.Event()
    release = threading.Event()
    attempts: dict[str, int] = {}
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args: Any) -> None:
            pass

        def do_POST(self) -> None:
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            scenario = request["messages"][-1]["content"]
            with lock:
                attempts[scenario] = attempts.get(scenario, 0) + 1
                attempt = attempts[scenario]
            if scenario == "cancelled":
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                self.wfile.write(("data: " + json.dumps({"id": "local-cancel", "object": "chat.completion.chunk", "model": request["model"], "choices": [{"index": 0, "delta": {"content": "started"}, "finish_reason": None}]}) + "\n\n").encode())
                self.wfile.flush()
                entered.set()
                assert release.wait(15), "test did not close cancelled stream"
                return
            if scenario == "blocked":
                entered.set()
                assert release.wait(15), "test did not release upstream barrier"
            if scenario == "failed" or scenario == "recovered" and attempt == 1:
                body = json.dumps({"error": {"type": "upstream_unavailable", "message": "local fixture failure"}}).encode()
                self.send_response(503)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            usage = {
                "prompt_tokens": 20, "completion_tokens": 7, "total_tokens": 27,
                "prompt_tokens_details": {"cached_tokens": 5},
                "completion_tokens_details": {"reasoning_tokens": 2},
            }
            if scenario == "unknown":
                usage = None
            elif scenario == "zero":
                usage = {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
                         "prompt_tokens_details": {"cached_tokens": 0},
                         "completion_tokens_details": {"reasoning_tokens": 0}}
            if request.get("stream"):
                frames = []
                for report in ([usage, usage, {"completion_tokens": 0}, {"prompt_tokens": None}] if scenario == "revised" else [usage]):
                    frames.append({"id": "local-usage", "object": "chat.completion.chunk", "model": request["model"],
                                   "choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": None}],
                                   **({"usage": report} if report is not None else {})})
                frames.append({"id": "local-usage", "object": "chat.completion.chunk", "model": request["model"],
                               "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
                body = ("".join("data: " + json.dumps(frame) + "\n\n" for frame in frames) + "data: [DONE]\n\n").encode()
                content_type = "text/event-stream"
            else:
                body = json.dumps({"id": "local-usage", "object": "chat.completion", "model": request["model"],
                                   "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                                   **({"usage": usage} if usage is not None else {})}).encode()
                content_type = "application/json"
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server, entered, release
    finally:
        release.set()
        server.shutdown()
        server.server_close()
        thread.join()


@pytest.fixture(params=["sqlite", "postgres"], ids=["sqlite", "postgres"])
def usage_env(request, stravia_binary: Path, storage_runtime: dict[str, Any], tmp_path: Path):
    backend = request.param
    schema = None
    database: dict[str, Any] = {"backend": "sqlite"}
    if backend == "postgres":
        if not storage_runtime["pg_url"]:
            pytest.fail("PostgreSQL stats contracts require isolated DB_URL")
        schema = storage_runtime["make_isolated_schema"]("stravia_usage_contract")
        storage_runtime["run_schema_action"]("create", pg_url=storage_runtime["pg_url"], schema=schema)
        database = {"backend": "postgres", "url": storage_runtime["postgres_dsn_for_schema"](storage_runtime["pg_url"], schema)}
    process = None
    logs = []
    try:
        with usage_upstream() as (upstream, entered, release):
            base = f"http://127.0.0.1:{find_free_port()}"
            args = ["--data-dir", str(tmp_path), "--host", "127.0.0.1", "--port", base.rsplit(":", 1)[1]]
            process, logs = start_stravia_server(stravia_binary=stravia_binary, args=args + storage_runtime["server_args"](backend))
            wait_until_ready(f"{base}/api/v1/auth/state")
            session = initialize_server(base, wait_for_setup_token(logs, process), database)
            yield {"admin": base, "proxy": base, "auth": session.auth_headers(),
                   "mock": f"http://127.0.0.1:{upstream.server_port}", "entered": entered, "release": release,
                   "backend": backend, "data_dir": tmp_path, "schema": schema,
                   "storage_runtime": storage_runtime, "process": process}
    finally:
        if process is not None:
            stop_stravia_server(process, logs)
        if schema is not None:
            storage_runtime["run_schema_action"]("drop", pg_url=storage_runtime["pg_url"], schema=schema)


@contextmanager
def terminal_attempt_stats_fault(env):
    """Reject only the real summary credit for successful upstream attempts."""
    if env["backend"] == "postgres":
        runtime = env["storage_runtime"]
        arguments = {"pg_url": runtime["pg_url"], "schema": env["schema"]}
        runtime["run_schema_action"]("stats-attempt-fault-install", **arguments)
        try:
            yield
        finally:
            runtime["run_schema_action"]("stats-attempt-fault-remove", **arguments)
    else:
        # usage_env starts --data-dir=tmp_path; DataPaths::database derives db/gateway.db.
        database_path = env["data_dir"] / "db" / "gateway.db"
        assert database_path.is_file(), "initialized isolated SQLite database missing"
        with closing(sqlite3.connect(database_path)) as database:
            database.executescript("""
                CREATE TRIGGER stravia_test_stats_attempt_insert
                BEFORE INSERT ON stats_usage_buckets
                WHEN NEW.successful_attempt_count > 0
                BEGIN SELECT RAISE(ABORT, 'injected terminal attempt stats failure'); END;
                CREATE TRIGGER stravia_test_stats_attempt_update
                BEFORE UPDATE ON stats_usage_buckets
                WHEN NEW.successful_attempt_count > OLD.successful_attempt_count
                BEGIN SELECT RAISE(ABORT, 'injected terminal attempt stats failure'); END;
            """)
        try:
            yield
        finally:
            with closing(sqlite3.connect(database_path)) as database:
                database.executescript("""
                    DROP TRIGGER IF EXISTS stravia_test_stats_attempt_insert;
                    DROP TRIGGER IF EXISTS stravia_test_stats_attempt_update;
                """)


def test_stats_write_failure_rejects_public_purge_and_preserves_history(usage_env):
    env = usage_env
    route_id, key = _create_route(env, "usage-failure-purge", retry_budget=0)
    status, body = _proxy(env, key, "usage-failure-purge", [{"role": "user", "content": "baseline"}])
    assert status == 200 and body["choices"][0]["message"]["content"] == "ok", body
    baseline = _wait_for("baseline usage history", lambda: next((
        item for item in _route_interactions(env, route_id)
        if item["input_preview"] == "baseline" and item["status"] == "completed"
    ), None))
    before = _wait_for("baseline terminal usage", lambda: _finished_detail(env, baseline["id"]))
    assert before["interaction"]["usage"]["output_tokens"] == 7
    with terminal_attempt_stats_fault(env):
        status, body = _proxy(env, key, "usage-failure-purge", [{"role": "user", "content": "stats-failure"}])
        assert status == 200 and body["choices"][0]["message"]["content"] == "ok", body
        # Do not wait for a rolled-back terminal observation. The gap proves
        # the targeted stats-bearing transaction failed before invoking purge.
        failed = _wait_for("stats transaction persistence gap", lambda: next((
            item for item in _route_interactions(env, route_id)
            if item["input_preview"] == "stats-failure" and item["observation_gap"]
        ), None))
        status, error = http_request("DELETE", f"{env['admin']}/api/v1/observations/history", headers=env["auth"])
        assert status == 503, f"unsafe history purge returned HTTP {status}"
        assert error["code"] == "observation_unavailable"
        retained = _detail(env, baseline["id"])
        assert retained["interaction"]["usage"] == before["interaction"]["usage"]
        assert retained["runs"] == before["runs"], "failed purge deleted retained attempt facts"
        assert _detail(env, failed["id"])["interaction"]["observation_gap"]
        assert env["process"].poll() is None, "stats persistence failure stopped the host"
    status, body = _proxy(env, key, "usage-failure-purge", [{"role": "user", "content": "following"}])
    assert status == 200 and body["choices"][0]["message"]["content"] == "ok", body
    following = _wait_for("following successful inference", lambda: next((
        item for item in _route_interactions(env, route_id)
        if item["input_preview"] == "following" and item["status"] == "completed"
    ), None))
    detail = _wait_for("following terminal usage", lambda: _finished_detail(env, following["id"]))
    assert detail["interaction"]["usage"]["output_tokens"] == 7
    assert env["process"].poll() is None
    # Removing the injection does not fabricate repair of lost usage facts.
    status, error = http_request("DELETE", f"{env['admin']}/api/v1/observations/history", headers=env["auth"])
    assert status == 503
    assert error["code"] == "observation_unavailable"
    assert _detail(env, baseline["id"])["interaction"]["usage"] == before["interaction"]["usage"]


def stats(env, view="overview", **query):
    status, body = http_request("GET", f"{env['admin']}/api/v1/stats/{view}?{urlencode(query)}", headers=env["auth"])
    assert status == 200, body
    return body["data"]


def clear(env):
    status, body = http_request("DELETE", f"{env['admin']}/api/v1/observations/history", headers=env["auth"])
    assert status == 200, body


@pytest.mark.parametrize("scenario,input_tokens,output_tokens,cache_read,reasoning", [
    ("known", 15, 7, 5, 2), ("unknown", None, None, None, None),
    ("zero", 0, 0, 0, 0), ("revised", 15, 0, 5, 2),
])
def test_exact_known_unknown_zero_and_revised_usage(usage_env, scenario, input_tokens, output_tokens, cache_read, reasoning):
    env = usage_env
    _route, key = _create_route(env, "usage-values")
    status, body = _proxy(env, key, "usage-values", [{"role": "user", "content": scenario}],
                          body_extra={"stream": scenario == "revised"})
    assert status == 200, body
    clear(env)
    overview = stats(env)
    assert overview["total_requests"] == 1
    assert overview["error_count"] == 0
    assert overview["total_input_tokens"] == input_tokens
    assert overview["total_output_tokens"] == output_tokens
    assert overview["total_cache_read_tokens"] == cache_read
    assert overview["total_reasoning_tokens"] == reasoning
    assert overview["total_cache_write_tokens"] is None
    model = stats(env, "models")[0]
    api_key = stats(env, "api-keys")[0]
    assert model["request_count"] == api_key["request_count"] == 1
    assert model["total_input_tokens"] == api_key["total_input_tokens"] == input_tokens
    assert model["total_output_tokens"] == api_key["total_output_tokens"] == output_tokens
    if scenario == "unknown":
        assert overview["avg_output_tps"] is None
    if scenario == "zero":
        assert overview["avg_output_tps"] == 0


def test_retries_recovery_failure_and_rejection_are_request_facts(usage_env):
    env = usage_env
    _route, key = _create_route(env, "usage-outcomes", retry_budget=1)
    for scenario in ("recovered", "failed"):
        status, body = _proxy(env, key, "usage-outcomes", [{"role": "user", "content": scenario}])
        assert (status == 200 if scenario == "recovered" else status >= 500), body
    status, body = http_request("POST", f"{env['proxy']}/v1/chat/completions", payload={"model": "usage-outcomes", "messages": []})
    assert status == 401, body
    clear(env)
    overview = stats(env)
    assert overview["total_requests"] == 3
    assert overview["error_count"] == 2
    assert overview["total_input_tokens"] == 15
    assert overview["total_output_tokens"] == 7
    provider = stats(env, "providers")[0]
    assert provider["request_count"] == 2
    assert provider["error_count"] == 1
    assert stats(env, "models")[0]["request_count"] == 2
    assert stats(env, "api-keys")[0]["request_count"] == 2


def test_client_disconnect_does_not_invent_failure_or_tokens(usage_env):
    env = usage_env
    _route, key = _create_route(env, "usage-cancelled")
    connection = HTTPConnection("127.0.0.1", int(env["proxy"].rsplit(":", 1)[1]), timeout=15)
    try:
        connection.request("POST", "/v1/chat/completions", body=json.dumps({
            "model": "usage-cancelled", "messages": [{"role": "user", "content": "cancelled"}], "stream": True,
        }), headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"})
        response = connection.getresponse()
        assert response.status == 200
        assert response.readline().startswith(b"data: ")
        assert env["entered"].wait(10)
        response.close()
    finally:
        connection.close()
        env["release"].set()
    clear(env)
    overview = stats(env)
    assert overview["total_requests"] == 1
    assert overview["error_count"] == 0
    assert overview["total_input_tokens"] is None
    assert overview["total_output_tokens"] is None
    assert overview["avg_output_tps"] is None


def test_unknown_sample_does_not_mask_known_tokens_or_fabricate_tps(usage_env):
    env = usage_env
    _route, key = _create_route(env, "usage-partial")
    for scenario in ("known", "unknown"):
        status, body = _proxy(env, key, "usage-partial", [{"role": "user", "content": scenario}])
        assert status == 200, body
    clear(env)
    overview = stats(env)
    assert overview["total_requests"] == 2
    assert overview["total_input_tokens"] == 15
    assert overview["total_output_tokens"] == 7
    assert overview["total_cache_read_tokens"] == 5
    assert overview["avg_output_tps"] is None


def test_clear_during_inflight_request_keeps_both_contributions(usage_env):
    env = usage_env
    _route, key = _create_route(env, "usage-concurrent")
    status, body = _proxy(env, key, "usage-concurrent", [{"role": "user", "content": "known"}])
    assert status == 200, body
    with ThreadPoolExecutor(max_workers=1) as executor:
        pending = executor.submit(_proxy, env, key, "usage-concurrent", [{"role": "user", "content": "blocked"}])
        assert env["entered"].wait(10), "inflight request did not reach upstream"
        try:
            clear(env)
        finally:
            env["release"].set()
        status, body = pending.result(timeout=15)
        assert status == 200, body
    clear(env)
    overview = stats(env)
    assert overview["total_requests"] == 2
    assert overview["error_count"] == 0
    assert overview["total_input_tokens"] == 30
    assert overview["total_output_tokens"] == 14


def test_every_stats_view_rejects_invalid_absolute_window(usage_env):
    env = usage_env
    invalid_windows = [
        {"start_at": 900000}, {"end_at": 1800000},
        {"start_at": 1800000, "end_at": 900000},
        {"start_at": 900000, "end_at": 900000},
        {"start_at": 900001, "end_at": 1800000},
        {"start_at": 0, "end_at": 900000},
    ]
    for view in ("overview", "series", "models", "providers", "api-keys"):
        for query in invalid_windows:
            status, body = http_request("GET", f"{env['admin']}/api/v1/stats/{view}?{urlencode(query)}", headers=env["auth"])
            assert status == 400, (view, query, body)
        status, body = http_request("GET", f"{env['admin']}/api/v1/stats/{view}?hours=24", headers=env["auth"])
        assert status == 400, (view, body)
    for bucket in (0, -900, 1, 901):
        status, body = http_request("GET", f"{env['admin']}/api/v1/stats/series?bucket={bucket}", headers=env["auth"])
        assert status == 400, (bucket, body)


def test_deleted_recreated_and_renamed_resources_keep_distinct_history(usage_env):
    env = usage_env
    model_id, key = _create_route(env, "usage-identity")
    status, body = _proxy(env, key, "usage-identity", [{"role": "user", "content": "known"}])
    assert status == 200, body
    clear(env)
    provider_id = stats(env, "providers")[0]["provider_id"]
    key_id = stats(env, "api-keys")[0]["api_key_id"]
    assert stats(env, "models")[0]["model_id"] == model_id
    for resource, identity, update in [
        ("models", model_id, {"display_name": "renamed model"}),
        ("providers", provider_id, {"name": "renamed provider"}),
        ("api-keys", key_id, {"name": "renamed key"}),
    ]:
        status, body = http_request("PUT", f"{env['admin']}/api/v1/{resource}/{identity}", payload=update, headers=env["auth"])
        assert status == 200, body
    status, body = _proxy(env, key, "usage-identity", [{"role": "user", "content": "known"}])
    assert status == 200, body
    clear(env)
    renamed_history = {}
    for view, field, identity in [("models", "model_id", model_id), ("providers", "provider_id", provider_id), ("api-keys", "api_key_id", key_id)]:
        rows = stats(env, view)
        assert len(rows) == 1, rows
        assert rows[0][field] == identity
        assert rows[0]["request_count"] == 2
        assert rows[0]["deleted"] is False, rows[0]
        renamed_history[view] = rows[0]
    for resource, identity in [("api-keys", key_id), ("models", model_id), ("providers", provider_id)]:
        status, body = http_request("DELETE", f"{env['admin']}/api/v1/{resource}/{identity}", headers=env["auth"])
        assert status == 200, body
        status, body = http_request("GET", f"{env['admin']}/api/v1/{resource}/{identity}", headers=env["auth"])
        # Management lookups report missing configuration in an error envelope,
        # unlike inference and observation endpoints that use HTTP 404.
        missing_error = {
            "models": f"Route not found: {identity}",
            "providers": f"provider not found: {identity}",
            "api-keys": "api key not found",
        }[resource]
        assert status == 200, body
        assert body == {"error": missing_error}, body
    status, body = _proxy(env, key, "usage-identity", [{"role": "user", "content": "known"}])
    assert status == 401, body
    recreated_model, recreated_key = _create_route(env, "usage-identity")
    assert recreated_model != model_id
    status, body = _proxy(env, recreated_key, "usage-identity", [{"role": "user", "content": "known"}])
    assert status == 200, body
    clear(env)
    for view, field, old_identity in [("models", "model_id", model_id), ("providers", "provider_id", provider_id), ("api-keys", "api_key_id", key_id)]:
        rows = stats(env, view)
        assert len(rows) == 2, rows
        old = next(row for row in rows if row[field] == old_identity)
        new = next(row for row in rows if row[field] != old_identity)
        assert old["request_count"] == 2
        assert new["request_count"] == 1
        assert old["deleted"] is True, old
        assert new["deleted"] is False, new
        assert new[field] is not None, new
        label = {"models": "model", "providers": "provider", "api-keys": "api_key_name"}[view]
        assert old[label], old
        assert old[label] == renamed_history[view][label], old
        assert new[label] == {
            "models": "usage-identity display",
            "providers": "usage-identity-provider",
            "api-keys": "usage-identity-key",
        }[view], new
    overview = stats(env)
    assert overview["total_requests"] == 4
    assert overview["error_count"] == 1
    assert overview["total_input_tokens"] == 45
    assert overview["total_output_tokens"] == 21


def test_retention_setting_and_debug_cleanup_do_not_reset_usage(usage_env):
    env = usage_env
    status, body = http_request("PUT", f"{env['admin']}/api/v1/observations/debug", payload={"enabled": True, "confirmed": True}, headers=env["auth"])
    assert status == 200, body
    _route, key = _create_route(env, "usage-debug-lifecycle")
    status, body = _proxy(env, key, "usage-debug-lifecycle", [{"role": "user", "content": "known"}])
    assert status == 200, body
    status, body = http_request("DELETE", f"{env['admin']}/api/v1/observations/debug", headers=env["auth"])
    assert status == 200, body
    # Debug cleanup serializes observation writes without deleting request facts.
    before = {view: stats(env, view) for view in ("overview", "models", "providers", "api-keys")}
    assert before["overview"]["total_requests"] == 1
    assert before["overview"]["total_output_tokens"] == 7
    status, body = http_request("GET", f"{env['admin']}/api/v1/observations/debug", headers=env["auth"])
    assert status == 200, body
    assert body["data"]["retained_bytes"] == 0
    for days in (30, 7):
        status, body = http_request("PUT", f"{env['admin']}/api/v1/settings/log_retention_days", payload={"value": str(days)}, headers=env["auth"])
        assert status == 200, body
        assert {view: stats(env, view) for view in before} == before
    clear(env)
    assert {view: stats(env, view) for view in before} == before


def test_long_window_coarsens_without_truncating_current_usage(usage_env):
    env = usage_env
    _route, key = _create_route(env, "usage-long-window")
    status, body = _proxy(env, key, "usage-long-window", [{"role": "user", "content": "known"}])
    assert status == 200, body
    clear(env)
    end = int(time.time() * 1000) // 900000 * 900000 + 900000
    query = {"start_at": end - 900000 * 2048, "end_at": end}
    overview = stats(env, **query)
    series = stats(env, "series", bucket=900, **query)
    assert 1 <= len(series) <= 512
    assert sum(row["request_count"] for row in series) == overview["total_requests"] == 1
    assert sum(row["total_output_tokens"] or 0 for row in series) == overview["total_output_tokens"] == 7
    assert sum(row["total_input_tokens"] or 0 for row in series) == overview["total_input_tokens"] == 15
    assert all(query["start_at"] <= row["bucket_start"] < end for row in series)
    assert stats(env)["total_output_tokens"] == 7
    for view in ("models", "providers", "api-keys"):
        assert stats(env, view, **query)[0]["request_count"] == 1
    # Adjacent historical range excludes the request, without drifting to now.
    previous = {"start_at": query["start_at"] - 900000, "end_at": query["start_at"]}
    assert stats(env, **previous)["total_requests"] == 0
    assert stats(env, **previous)["total_requests"] == 0
