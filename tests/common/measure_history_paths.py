"""Deterministic, isolated real-HTTP history-path benchmark (not a pytest test).

Run as ``python -m tests.common.measure_history_paths``. Samples are raw, not
estimates; invalid/unexercised paths fail closed. Use identical arguments for
both frozen binaries. Compact windows must match the fake provider's actual
output semantics; rejected-session operations must have an observed null parent
as well as multi-candidate discovery traces. An independent prefix compact case
adds a new user to the penultimate generation and must find that successful source
after scanning newer same-depth decoys; it does not replace the exact-window case.
No protocol-matrix coverage is implied.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import platform
import re
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlencode, urlsplit

from tests.common.helpers import (
    CompleteRequestHTTPConnection, initialize_server, start_stravia_server,
    stop_stravia_server, wait_for_setup_token,
)
from tests.common.measure_protocol_http import (
    Catalog, ProcessReader, database_config, encode, response_frames, response_text,
)


def semantics(items):
    """Same native-window normalization as admin/test_compaction.py."""
    items = json.loads(json.dumps(items))
    for item in items:
        if item.get("role"):
            item.setdefault("type", "message")
        if item.get("type") != "message":
            continue
        if isinstance(item.get("content"), str):
            item["content"] = [{"type": "output_text" if item["role"] == "assistant" else "input_text", "text": item["content"]}]
        for part in item.get("content", []):
            if part.get("annotations") == []:
                del part["annotations"]
        for key in ("id", "status"):
            item.pop(key, None)
    return items


def compact_semantics(items):
    """Preserve the full native window except Gateway-assigned item identities."""
    items = semantics(items)
    for item in items:
        item.pop("id", None)
    return items


def compact_digest(items):
    """Ignore JSON object key order while preserving window and content order."""
    canonical = json.dumps(compact_semantics(items), sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


class NativeProvider(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        items = body.get("input", [])
        with self.server.lock:
            expectation = self.server.expected
            previous_id = body.get("previous_response_id")
            previous_history = self.server.response_histories.get(previous_id) if previous_id else None
        effective_items = (previous_history + items) if previous_history is not None else items
        error_code = "unknown_previous_response" if previous_id and previous_history is None else None
        if semantics(effective_items) != semantics(expectation["input"]):
            error_code = error_code or "synthetic_history_mismatch"
        compact = self.path == "/v1/responses/compact"
        if self.path not in ("/v1/responses", "/v1/responses/compact"):
            error_code = error_code or "synthetic_unsupported_path"
        valid = error_code is None
        signature = hashlib.sha256(encode(semantics(effective_items))).hexdigest()[:24]
        text = expectation["text"]
        if compact:
            signature = hashlib.sha256(encode({"input": semantics(effective_items), "instructions": body.get("instructions")})).hexdigest()[:24]
            window = [
                {"type": "message", "role": "user", "content": "retained task identity"},
                {"type": "compaction", "id": "cmp_" + signature, "encrypted_content": "opaque-" + signature},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "retained answer", "annotations": []}]},
            ]
            result = {"id": "cmp_operation_" + signature, "object": "response.compaction", "created_at": 1788825600, "output": window}
        else:
            result = response_frames("open-responses", "native-model", text, False)[0][1]
            result["id"] = "resp_" + signature
            result["output"][0]["id"] = "msg_" + signature
        if valid and not compact:
            with self.server.lock:
                self.server.response_histories[result["id"]] = json.loads(json.dumps(effective_items + result["output"]))
        if not valid:
            result = {"error": {"code": error_code}}
        data = encode(result)
        self.send_response(200 if valid else 400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        with self.server.lock:
            self.server.arrivals.append({"path": self.path, "valid": valid, "error_code": error_code,
                                         "native_previous_response": bool(previous_id), "previous_response_known": previous_history is not None if previous_id else None,
                                         "input_sha256": hashlib.sha256(encode(semantics(items))).hexdigest(), "input_items": len(items),
                                         "effective_input_sha256": hashlib.sha256(encode(semantics(effective_items))).hexdigest(), "effective_input_items": len(effective_items),
                                         # Digest the fake provider's actual output, not the Gateway response.
                                         # This runs after the write, outside the client request timer.
                                         "compact_output_sha256": compact_digest(result["output"]) if compact and valid else None})


def admin(session, method, path, body=None, expected=200):
    status, value = session.request(method, path, body, timeout=60)
    if status != expected:
        # Response bodies can contain credentials; never include them in errors.
        raise RuntimeError(f"admin {method} {path}: HTTP {status}")
    return value["data"]


def provision(session, port, suffix):
    targets = []
    for index in range(2):
        provider = admin(session, "POST", "/api/v1/providers", {
            "name": f"history-{suffix}-{index}",
            "source": {"type": "custom", "vendor": "custom", "channel": "default", "protocol": "open-responses", "base_url": f"http://127.0.0.1:{port}/v1"},
            "credential": {"type": "api_key", "value": "local-test-credential"}, "vendor_options": {},
        })
        admin(session, "POST", f"/api/v1/providers/{provider['id']}/models", {
            "model_id": "native-model", "metadata": {"name": "Native benchmark", "reasoning_efforts": ["none", "low", "medium", "high"], "limit": {"context": 100000}},
        }, 201)
        targets.append({"provider_id": provider["id"], "model": "native-model", "enabled": True, "priority": index, "target_retry_budget": 0})
    model = "history-" + suffix
    route = admin(session, "POST", "/api/v1/models", {"model_id": model, "targets": targets})
    key = admin(session, "POST", "/api/v1/api-keys", {"name": model, "model_ids": [route["id"]]})["key"]
    return model, route["id"], key


def observe(session, route, generation, directory, exclude_runs=()):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        query = urlencode({"model": route, "anchor_at": int(time.time() * 1000) + 1000, "window_index": 0, "limit": 100})
        forest = admin(session, "GET", "/api/v1/observations/interactions?" + query)
        for root in forest["roots"]:
            for interaction in root["interactions"]:
                detail = admin(session, "GET", "/api/v1/observations/interactions/" + interaction["id"])
                for run in detail["runs"]:
                    if run["id"] in exclude_runs or run.get("generation_node_id") != generation:
                        continue
                    trace = run.get("trace") or {}
                    if trace.get("status") != "complete":
                        continue
                    return run
        time.sleep(.1)
    raise RuntimeError("completed Generation observation/trace not available")


def send(base, key, model, items, upstream, case, expected_parent, text, previous=None, compact=False):
    body = {"model": model, "input": items}
    if previous:
        body["previous_response_id"] = previous
    if "instructions" in case:
        body["instructions"] = case["instructions"]
    with upstream.lock:
        upstream.expected = {"input": case["upstream_input"], "text": text}
        arrivals_start = len(upstream.arrivals)
    address = urlsplit(base)
    connection = CompleteRequestHTTPConnection(address.hostname, address.port, timeout=120)
    raw_body = encode(body)
    started = time.perf_counter()
    row = {"case": case["name"], "round": case["round"], "expected_parent": expected_parent, "request_bytes": len(raw_body), "input_sha256": hashlib.sha256(encode(items)).hexdigest(), "status": None, "terminal_valid": False, "content_valid": False, "error": None, "first_content_ms": None, "first_content_semantics": "unary full response available"}
    result = None
    try:
        connection.request("POST", "/v1/responses" + ("/compact" if compact else ""), raw_body, {
            "Authorization": "Bearer " + key, "Content-Type": "application/json",
            # Public ingress contract: enables session candidate indexing by
            # semantic-unit count before canonical-history verification.
            "x-session-id": "history-paths-v1:" + model,
        })
        response = connection.getresponse()
        row["status"] = response.status
        raw = response.read()
        row["latency_ms"] = (time.perf_counter() - started) * 1000
        result = json.loads(raw)
        error = result.get("error") if isinstance(result, dict) else None
        code = error.get("code") if isinstance(error, dict) else None
        safe_codes = {"synthetic_history_mismatch", "unknown_previous_response", "synthetic_unsupported_path",
                      "invalid_previous_response_id", "invalid_request_error", "upstream_error", "upstream_request_failed",
                      "parent_generation_not_found", "generation_not_found", "invalid_compacted_window"}
        row["response_error_code"] = code if isinstance(code, str) and code in safe_codes else ("unrecognized_error_code" if code is not None else None)
        if compact:
            row["terminal_valid"] = result.get("object") == "response.compaction" and bool(result.get("output"))
            # Compare against this provider arrival after synchronization below.
            row["content_valid"] = False
        else:
            visible = response_text("open-responses", raw, False)
            row["terminal_valid"] = (
                result.get("object") == "response"
                and result.get("status") == "completed"
                and result.get("error") is None
                and result.get("incomplete_details") is None
                and isinstance(result.get("id"), str) and bool(result["id"])
                and isinstance(result.get("output"), list)
                and bool(result["output"])
                and all(item.get("status") == "completed" for item in result["output"] if item.get("type") == "message")
            )
            row["content_valid"] = visible == text
            if visible:
                row["first_content_ms"] = row["latency_ms"]
        row["generation_id"] = result.get("id")
    except Exception as error:
        row["error"] = type(error).__name__
    finally:
        row.setdefault("latency_ms", (time.perf_counter() - started) * 1000)
        connection.close()
    # Handler appends after write; synchronize without mistaking a scheduling race for failure.
    deadline = time.monotonic() + 5
    while len(upstream.arrivals) == arrivals_start and time.monotonic() < deadline:
        time.sleep(.005)
    with upstream.lock:
        row["upstream"] = list(upstream.arrivals[arrivals_start:])
    if compact and isinstance(result, dict) and isinstance(result.get("output"), list) and len(row["upstream"]) == 1:
        expected_sha256 = row["upstream"][0].get("compact_output_sha256")
        row["compact_output_sha256"] = compact_digest(result["output"])
        row["content_valid"] = expected_sha256 is not None and row["compact_output_sha256"] == expected_sha256
    row["valid"] = row["status"] == 200 and row["terminal_valid"] and row["content_valid"] and not row["error"] and len(row["upstream"]) == 1 and row["upstream"][0]["valid"]
    return row, result


def timeline(session):
    status, value = session.request("GET", "/api/v1/performance/timeline", timeout=60)
    if status != 200:
        raise RuntimeError(f"performance timeline HTTP {status}")
    return value


def trace_key(event):
    return (event.get("pid"), event.get("tid"), event.get("ts"), event.get("name"), event.get("ph"))


def attach_performance(session, row, before):
    snapshot = timeline(session)
    prior = {trace_key(event) for event in before.get("traceEvents", []) if event.get("ph") == "X"}
    events = []
    for event in snapshot.get("traceEvents", []):
        if event.get("ph") != "X" or trace_key(event) in prior:
            continue
        # Keep stage names and numeric attribution only: never serialize auth,
        # provider credentials, request content, or debug wire payloads.
        args = {key: value for key, value in event.get("args", {}).items()
                if isinstance(value, (int, float, bool)) and not any(word in key.lower() for word in ("token", "key", "secret"))}
        events.append({"name": event["name"], "ts_us": event.get("ts"), "dur_us": event.get("dur"), "args": args,
                       "sql_count": args.get("sql_query_count", 0), "sql_duration_us": args.get("sql_query_duration_us", 0)})
    row["performance_spans"] = events
    metadata_fields = ("dropped_active", "dropped_completed", "incomplete", "incomplete_total")
    row["trace_metadata_before"] = {key: before.get("metadata", {}).get(key) for key in metadata_fields}
    row["trace_metadata_after"] = {key: snapshot.get("metadata", {}).get(key) for key in metadata_fields}
    row["trace_metadata_valid"] = all(
        isinstance(row["trace_metadata_before"][key], int)
        and isinstance(row["trace_metadata_after"][key], int)
        and row["trace_metadata_after"][key] <= row["trace_metadata_before"][key]
        for key in metadata_fields
    )
    prior_all = {trace_key(event) for event in before.get("traceEvents", [])}
    fresh_incomplete = [event["name"] for event in snapshot.get("traceEvents", [])
                        if event.get("ph") == "B" and trace_key(event) not in prior_all]
    row["fresh_incomplete_spans"] = fresh_incomplete
    row["trace_metadata_valid"] = row["trace_metadata_valid"] and not fresh_incomplete
    row["request_span_valid"] = any(
        span["name"].startswith("router.select") or span["name"] == "generation_chain.history.persist"
        for span in events
    )
    row["trace_attribution"] = "serial request interval, including observation/admin polling; span-local SQL counts are not summed across nested spans"


def finalize_validity(result, args):
    rows = result["requests"]
    if any(not row["valid"] for row in rows):
        result["failure_reasons"].append("HTTP, parent, terminal or content validation failed")
    expected = args.repetitions * (3 * args.rounds + args.candidates + 4)
    if len(rows) != expected:
        result["failure_reasons"].append(f"missing requests: {len(rows)} of {expected}")
    compact_rows = [row for row in rows if row["case"] == "native-compact-operation"]
    if len(compact_rows) != args.repetitions:
        result["failure_reasons"].append(f"missing native compact operations: {len(compact_rows)} of {args.repetitions}")
    evidence = []
    for row in compact_rows:
        spans = [span for span in row.get("performance_spans", []) if span["name"] == "generation_chain.parent.discover_prefix"]
        candidate_counts = [span["args"].get("candidate_count", 0) for span in spans]
        valid = len(spans) >= 2 and max(candidate_counts, default=0) >= max(2, args.candidates)
        evidence.append({"repetition": row["repetition"], "discovery_span_count": len(spans), "candidate_counts": candidate_counts, "valid": valid})
        if not valid:
            result["failure_reasons"].append("native double discovery/multiple history candidates not evidenced by trace")
    result["native_discovery_evidence"] = evidence
    prefix_rows = [row for row in rows if row["case"] == "native-prefix-compact-operation"]
    if len(prefix_rows) != args.repetitions:
        result["failure_reasons"].append(f"missing native prefix compact operations: {len(prefix_rows)} of {args.repetitions}")
    prefix_evidence = []
    for row in prefix_rows:
        spans = [span for span in row.get("performance_spans", []) if span["name"] == "generation_chain.parent.discover_prefix"]
        candidate_counts = [span["args"].get("candidate_count", 0) for span in spans]
        valid = row.get("parent_valid", False) and len(spans) >= 2 and all(count >= args.candidates + 1 for count in candidate_counts)
        prefix_evidence.append({"repetition": row["repetition"], "discovery_span_count": len(spans), "candidate_counts": candidate_counts,
                                "expected_parent": row["expected_parent"], "observed_parent": row.get("observed_parent"),
                                "parent_valid": row.get("parent_valid", False), "valid": valid})
        if not valid:
            result["failure_reasons"].append("native prefix case lacks its successful source parent or two scans covering newer decoys plus source")
    result["native_prefix_discovery_evidence"] = prefix_evidence
    rejected_rows = [row for row in rows if row["case"] == "rejected-session-candidates"]
    if len(rejected_rows) != args.repetitions:
        result["failure_reasons"].append(f"missing rejected-session operations: {len(rejected_rows)} of {args.repetitions}")
    rejected_evidence = []
    for row in rejected_rows:
        spans = [span for span in row.get("performance_spans", []) if span["name"] == "generation_chain.parent.discover_prefix"]
        candidate_counts = [span["args"].get("candidate_count", 0) for span in spans]
        valid = row.get("parent_valid", False) and len(spans) >= 2 and all(count >= max(2, args.candidates) for count in candidate_counts)
        rejected_evidence.append({"repetition": row["repetition"], "discovery_span_count": len(spans), "candidate_counts": candidate_counts,
                                  "observed_parent": row.get("observed_parent"), "parent_valid": row.get("parent_valid", False), "valid": valid})
        if not valid:
            result["failure_reasons"].append("rejected-session case lacks a verified standalone parent or two multi-candidate discovery scans")
    result["rejected_session_discovery_evidence"] = rejected_evidence
    if any(not row.get("performance_spans") for row in rows):
        result["failure_reasons"].append("missing per-request performance spans")
    if any(not row.get("trace_metadata_valid") for row in rows):
        result["failure_reasons"].append("trace metadata missing or dropped/incomplete spans during request window")
    if any(not row.get("request_span_valid") for row in rows):
        result["failure_reasons"].append("no request-specific routing/history span in timeline diff")
    if result["rss_peak_bytes"] is None:
        result["failure_reasons"].append("no server RSS samples")
    result["valid"] = not result["failure_reasons"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--backend", choices=("sqlite", "postgres"), default="sqlite")
    parser.add_argument("--database-url")
    parser.add_argument("--redis-url", help="Dedicated loopback Redis for PostgreSQL runtime cache")
    parser.add_argument("--confirm-isolated-database", action="store_true",
                        help="Confirm database and any supplied Redis are dedicated isolated benchmark resources")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--rounds", type=int, default=8)
    parser.add_argument("--candidates", type=int, default=48)
    parser.add_argument("--prompt-bytes", type=int, default=8192)
    parser.add_argument("--sample-interval", type=float, default=.05)
    args = parser.parse_args()
    if args.repetitions < 1 or args.rounds < 2 or args.candidates < 2 or args.prompt_bytes < 128 or args.sample_interval <= 0:
        parser.error("repetitions >=1, rounds >=2, candidates >=2, prompt-bytes >=128 and positive sample-interval required")
    if args.backend == "postgres" and args.repetitions != 1:
        parser.error("Postgres requires --repetitions 1 and a fresh dedicated database per invocation")
    if args.backend == "postgres" and not args.redis_url:
        parser.error("Postgres requires --redis-url for its dedicated loopback runtime cache")
    if args.redis_url:
        redis = urlsplit(args.redis_url)
        if redis.scheme not in ("redis", "rediss") or redis.hostname not in ("127.0.0.1", "localhost", "::1") or not args.confirm_isolated_database:
            parser.error("Redis requires a loopback redis/rediss URL and --confirm-isolated-database; never supply production storage")
    database = database_config(args)
    binary = args.binary.resolve()
    with binary.open("rb") as source:
        sha = hashlib.file_digest(source, "sha256").hexdigest()
    metadata = {name: str(value) if isinstance(value, Path) else value for name, value in vars(args).items()}
    metadata["database_url"] = "isolated loopback database (credentials omitted)" if args.database_url else None
    metadata["redis_url"] = "isolated loopback Redis (credentials omitted)" if args.redis_url else None
    result = {"schema_version": 1, "label": args.label, "binary_sha256": sha, "platform": platform.platform(), "workload": metadata, "seed": "history-paths-v1", "session_header": "x-session-id=history-paths-v1:<model>; fixed per route across root, candidates and compact", "compaction_source": "canonical full history without response item identities; same controls as source generations", "native_prefix_compaction_source": "independent successful penultimate-generation prefix plus a deterministic new user; newer same-depth decoys are rejected in both discovery scans before matching that source; not the original exact-window case", "candidate_depth": "main chain minus one round; distinct content in shared session namespace", "rejected_session_case": "unchanged canonical full history with legal mismatched instructions; both discoveries must scan requested candidate count", "cache": {"capacity_mb": 0}, "performance_debug": True, "ordinary_observation_history": True, "samples": [], "requests": [], "valid": False, "failure_reasons": []}
    padding = "".join(hashlib.sha256(f"history-paths-v1:{i}".encode()).hexdigest() for i in range((args.prompt_bytes + 63) // 64))[:args.prompt_bytes]
    upstream = ThreadingHTTPServer(("127.0.0.1", 0), NativeProvider)
    upstream.lock = threading.Lock()
    upstream.expected = {}
    upstream.response_histories = {}
    upstream.arrivals = []
    catalog = ThreadingHTTPServer(("127.0.0.1", 0), Catalog)
    catalog.arrivals = []
    threads = []
    for server in (upstream, catalog):
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        threads.append(thread)
    stage = "resource-startup"
    try:
        for repetition in range(args.repetitions):
            with tempfile.TemporaryDirectory(prefix="stravia-history-bench-") as directory:
                process = None
                logs = []
                stop = threading.Event()
                sampler = reader = None
                try:
                    config_path = Path(directory) / "benchmark.toml"
                    config_text = "[cache]\ncapacity_mb = 0\n"
                    if args.redis_url:
                        config_text += "redis_url = " + json.dumps(args.redis_url) + "\n"
                    config_path.write_text(config_text, encoding="utf-8")
                    command = ["--host", "127.0.0.1", "--port", "0", "--data-dir", directory, "--config", str(config_path), "--test-catalog-base-url", f"http://127.0.0.1:{catalog.server_port}"]
                    stage = f"repetition-{repetition}:server-start"
                    process, logs = start_stravia_server(stravia_binary=binary, args=command, env={"RUST_LOG": "warn,stravia_server=info"})
                    stage = f"repetition-{repetition}:setup-token"
                    token = wait_for_setup_token(logs, process, timeout=180)
                    listener = next(re.search(r"Stravia startup listener opened .*?(127\.0\.0\.1:\d+)", line).group(1) for line in logs if re.search(r"Stravia startup listener opened .*?(127\.0\.0\.1:\d+)", line))
                    base = "http://" + listener
                    stage = f"repetition-{repetition}:http-setup"
                    session = initialize_server(base, token, database)
                    stage = f"repetition-{repetition}:debug-enable"
                    admin(session, "PUT", "/api/v1/observations/debug", {"enabled": True, "confirmed": True})
                    reader = ProcessReader(process.pid)
                    def sample():
                        while not stop.is_set():
                            try:
                                result["samples"].append({"repetition": repetition, "timestamp": time.time(), **reader.read()})
                            except Exception as error:
                                result["failure_reasons"].append("RSS sampler: " + type(error).__name__)
                                return
                            stop.wait(args.sample_interval)
                    sampler = threading.Thread(target=sample, daemon=True)
                    sampler.start()
                    for mode in ("explicit-parent", "automatic-full-history", "native-compaction"):
                        stage = f"repetition-{repetition}:{mode}:provision"
                        model, route, key = provision(session, upstream.server_port, f"{repetition}-{mode}")
                        history = []
                        parent = None
                        main_chain_ids = []
                        main_chain_histories = []
                        for round_index in range(args.rounds):
                            stage = f"repetition-{repetition}:{mode}:round-{round_index}"
                            user = {"role": "user", "content": f"{mode}:{round_index}:" + padding}
                            text = "answer-" + hashlib.sha256(user["content"].encode()).hexdigest()
                            full = history + [user]
                            items = [user] if mode == "explicit-parent" else full
                            before = timeline(session)
                            row, response = send(base, key, model, items, upstream, {"name": mode, "round": round_index, "upstream_input": full}, parent, text, previous=parent if mode == "explicit-parent" else None)
                            row.update(repetition=repetition, route_id=route)
                            result["requests"].append(row)
                            if not row["valid"]:
                                raise RuntimeError("generation HTTP/content/upstream validation failed")
                            run = observe(session, route, response["id"], directory)
                            row["observed_parent"] = run.get("generation_parent_id")
                            row["parent_valid"] = row["observed_parent"] == parent
                            row["valid"] = row["valid"] and row["parent_valid"]
                            attach_performance(session, row, before)
                            history = full + response["output"]
                            parent = response["id"]
                            main_chain_ids.append(parent)
                            main_chain_histories.append(history)
                        if mode == "native-compaction":
                            branch_parent = main_chain_ids[-3] if args.rounds >= 3 else None
                            branch_prefix = main_chain_histories[-2][:-2]
                            for candidate in range(args.candidates):
                                stage = f"repetition-{repetition}:native-candidate-{candidate}"
                                decoy_user = {"role": "user", "content": f"native-compaction:decoy-{candidate}:" + padding}
                                decoy_input = branch_prefix + [decoy_user]
                                decoy_text = "answer-" + hashlib.sha256(decoy_user["content"].encode()).hexdigest()
                                before = timeline(session)
                                decoy_row, decoy_response = send(base, key, model, decoy_input, upstream, {"name": "native-discovery-candidate", "round": candidate, "upstream_input": decoy_input}, branch_parent, decoy_text)
                                decoy_row.update(repetition=repetition, route_id=route)
                                result["requests"].append(decoy_row)
                                if not decoy_row["valid"]:
                                    raise RuntimeError("native candidate generation validation failed")
                                decoy_run = observe(session, route, decoy_response["id"], directory)
                                decoy_row["observed_parent"] = decoy_run.get("generation_parent_id")
                                decoy_row["parent_valid"] = decoy_row["observed_parent"] == branch_parent
                                decoy_row["valid"] = decoy_row["valid"] and decoy_row["parent_valid"]
                                attach_performance(session, decoy_row, before)
                            # Independent successful prefix-source case: append a new user
                            # to the penultimate generation, not the original exact window.
                            stage = f"repetition-{repetition}:native-prefix-compact-operation"
                            prefix_parent = main_chain_ids[-2]
                            prefix_user = {"role": "user", "content": "native-compaction:prefix-new-user:" + padding}
                            prefix_input = semantics(main_chain_histories[-2] + [prefix_user])
                            before = timeline(session)
                            prefix_row, _ = send(
                                base, key, model, prefix_input, upstream,
                                {"name": "native-prefix-compact-operation", "round": args.rounds,
                                 "upstream_input": prefix_input}, prefix_parent, "", compact=True,
                            )
                            prefix_row.update(repetition=repetition, route_id=route,
                                              parent_validation="successful penultimate-generation prefix plus a new user; newer same-depth decoys must be rejected")
                            result["requests"].append(prefix_row)
                            if not prefix_row["valid"]:
                                raise RuntimeError("native prefix compact HTTP/content validation failed")
                            prefix_run = observe(session, route, None, directory)
                            prefix_row["observation_run_id"] = prefix_run["id"]
                            prefix_row["observed_parent"] = prefix_run.get("generation_parent_id")
                            prefix_row["parent_valid"] = prefix_row["observed_parent"] == prefix_parent
                            prefix_row["valid"] = prefix_row["valid"] and prefix_row["parent_valid"]
                            attach_performance(session, prefix_row, before)
                            stage = f"repetition-{repetition}:native-compact-operation"
                            compact_input = semantics(history)
                            before = timeline(session)
                            row, compacted = send(base, key, model, compact_input, upstream, {"name": "native-compact-operation", "round": args.rounds, "upstream_input": compact_input}, parent, "", compact=True)
                            row.update(repetition=repetition, route_id=route)
                            result["requests"].append(row)
                            if not row["valid"]:
                                raise RuntimeError("native compact operation validation failed")
                            compact_run = observe(session, route, None, directory, exclude_runs={prefix_run["id"]})
                            row["observation_run_id"] = compact_run["id"]
                            attach_performance(session, row, before)
                            user = {"role": "user", "content": "continue compacted task:" + padding}
                            full = compacted["output"] + [user]
                            text = "continued from compacted window"
                            stage = f"repetition-{repetition}:native-compacted-continuation"
                            before = timeline(session)
                            row, response = send(base, key, model, full, upstream, {"name": "native-compacted-continuation", "round": args.rounds + 1, "upstream_input": full}, parent, text)
                            row.update(repetition=repetition, route_id=route)
                            result["requests"].append(row)
                            if not row["valid"]:
                                raise RuntimeError("native continuation validation failed")
                            run = observe(session, route, response["id"], directory)
                            row["observed_parent"] = run.get("generation_parent_id")
                            row["parent_valid"] = row["observed_parent"] == parent
                            row["valid"] = row["valid"] and row["parent_valid"]
                            attach_performance(session, row, before)
                            # Deliberately mismatch legal controls, not content or
                            # session identity: both discovery scans must reject
                            # every stored session candidate after materializing it.
                            stage = f"repetition-{repetition}:rejected-session-candidates"
                            before = timeline(session)
                            rejected_row, _ = send(
                                base, key, model, compact_input, upstream,
                                {"name": "rejected-session-candidates", "round": args.rounds + 2,
                                 "upstream_input": compact_input,
                                 "instructions": "history benchmark: independently compact this unchanged window"},
                                None, "", compact=True,
                            )
                            rejected_row.update(repetition=repetition, route_id=route,
                                                parent_validation="standalone control-mismatch operation; not a successful source continuation")
                            result["requests"].append(rejected_row)
                            if not rejected_row["valid"]:
                                raise RuntimeError("rejected session candidate compact HTTP/content validation failed")
                            rejected_run = observe(session, route, None, directory, exclude_runs={prefix_run["id"], compact_run["id"]})
                            rejected_row["observation_run_id"] = rejected_run["id"]
                            rejected_row["observed_parent"] = rejected_run.get("generation_parent_id")
                            rejected_row["parent_valid"] = "generation_parent_id" in rejected_run and rejected_row["observed_parent"] is None
                            rejected_row["valid"] = rejected_row["valid"] and rejected_row["parent_valid"]
                            attach_performance(session, rejected_row, before)
                finally:
                    stop.set()
                    if sampler:
                        sampler.join(timeout=5)
                    if reader:
                        reader.close()
                    if process:
                        stop_stravia_server(process, logs, print_tail=0)
    except Exception as error:
        result["failure_reasons"].append(f"stage={stage}: {type(error).__name__} (exception body omitted to protect credentials)")
    finally:
        for server in (upstream, catalog):
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=5)
        result["catalog_arrivals"] = catalog.arrivals
        result["rss_peak_bytes"] = max((sample["rss_bytes"] for sample in result["samples"]), default=None)
        finalize_validity(result, args)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    return 0 if result["valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
