from __future__ import annotations

from copy import deepcopy

import pytest

from tests.common.measure_history_paths import attach_performance_snapshot


def span(identifier, name, parent=None, state="finished"):
    args = {"span_id": identifier, "capture_state": state, "status": "completed"}
    if parent is not None:
        args["parent_id"] = parent
    event = {"pid": 1, "tid": 1, "ts": identifier * 10, "name": name,
             "ph": "X" if state == "finished" else "B", "args": args}
    if state == "finished":
        event["dur"] = 5
    return event


def snapshot(*events):
    active = sum(event["args"]["capture_state"] == "active" for event in events)
    frozen = sum(event["args"]["capture_state"] == "frozen" for event in events)
    return {
        "traceEvents": list(events),
        "metadata": {
            "dropped_active": 0, "dropped_completed": 0, "dropped_sql_completed": 0,
            "dropped_resource_samples": 0, "active_spans": active, "frozen_spans": frozen,
            "frozen_total": frozen, "incomplete": active + frozen, "incomplete_total": active + frozen,
            "schema_version": 3, "debug_epoch": 1, "debug_enabled": True,
            "storage_status": "ready", "storage_errors": 0,
        },
    }


def request_events():
    return [span(1, "proxy.inference_run"), span(2, "router.select", 1),
            span(3, "generation_chain.parent.resolve", 1), span(4, "sql.query", 3)]


def measure(after, before=None):
    row = {}
    attach_performance_snapshot(row, snapshot() if before is None else before, after)
    return row


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("parent", [1, 3, 4])
def test_history_benchmark_rejects_unfinished_request_descendant(parent):
    row = measure(snapshot(*request_events(), span(5, "sql.query", parent, "active")))
    assert not row["trace_metadata_valid"]
    assert not row["request_span_valid"]
    assert [event["span_id"] for event in row["active_request_spans"]] == [5]
    assert row["request_sql_span_count"] == 1
    assert row["request_sql_duration_us"] == 5


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("other_root", [False, True])
def test_history_benchmark_reports_unrelated_live_sql_without_charging_request(other_root):
    unrelated = [span(10, "proxy.inference_run")] if other_root else []
    before = snapshot(*unrelated)
    after = snapshot(*unrelated, *request_events(),
                     span(11, "sql.query", 10 if other_root else None, "active"))
    row = measure(after, before)
    assert row["trace_metadata_valid"]
    assert row["request_span_valid"]
    assert row["trace_metadata_after"]["incomplete_total"] > row["trace_metadata_before"]["incomplete_total"]
    assert row["global_incomplete_spans"] == ["sql.query"]
    assert row["active_unrelated_spans"] == [{
        "name": "sql.query", "span_id": 11, "parent_id": 10 if other_root else None,
        "attribution": "other_inference" if other_root else "unrelated_sql",
    }]
    assert row["request_sql_span_count"] == 1
    assert row["request_sql_duration_us"] == 5


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("command", [False, True])
def test_history_benchmark_separates_observation_causal_chain(command):
    background = span(10, "observation.command.process" if command else "observation.flush",
                      1 if command else None, "active")
    after = snapshot(*request_events(), background,
                     span(11, "turn_chain.persist", 10),
                     span(12, "sql.query", 11, "active"))
    row = measure(after)
    assert row["trace_metadata_valid"]
    assert row["request_span_valid"]
    assert [event["span_id"] for event in row["active_observation_spans"]] == [10, 12]
    assert [event["name"] for event in row["observation_history_spans"]] == ["turn_chain.persist"]
    assert [event["name"] for event in row["history_measurements"]] == ["generation_chain.parent.resolve"]
    assert row["request_sql_span_count"] == 1
    assert row["request_subtree_span_count"] == 4


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("field", [
    "dropped_active", "dropped_completed", "dropped_sql_completed", "dropped_resource_samples",
    "frozen_spans", "frozen_total", "storage_errors",
])
def test_history_benchmark_rejects_growing_loss_counters(field):
    after = snapshot(*request_events())
    after["metadata"][field] += 1
    assert not measure(after)["trace_metadata_valid"]


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("field", ["active_spans", "frozen_spans", "frozen_total"])
@pytest.mark.parametrize("side", ["before", "after"])
def test_history_benchmark_requires_lifecycle_counters(field, side):
    before, after = snapshot(), snapshot(*request_events())
    del (before if side == "before" else after)["metadata"][field]
    assert not measure(after, before)["trace_metadata_valid"]


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("state", ["frozen", "unknown", None])
def test_history_benchmark_rejects_non_live_begin_states(state):
    after = snapshot(*request_events(), span(10, "sql.query", state="active"))
    if state is None:
        del after["traceEvents"][-1]["args"]["capture_state"]
    else:
        after["traceEvents"][-1]["args"]["capture_state"] = state
    row = measure(after)
    assert not row["trace_metadata_valid"]
    assert row["trace_capture_errors"]


@pytest.mark.e2e
@pytest.mark.proxy
def test_history_benchmark_rejects_retained_frozen_span_without_counter_growth():
    frozen = span(10, "sql.query", state="frozen")
    row = measure(snapshot(*request_events(), frozen), snapshot(deepcopy(frozen)))
    assert row["trace_metadata_before"]["frozen_total"] == row["trace_metadata_after"]["frozen_total"] == 1
    assert not row["trace_metadata_valid"]
    assert any("frozen span" in error for error in row["trace_capture_errors"])


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("state", ["active", "frozen", None])
def test_history_benchmark_requires_finished_capture_state_for_complete_events(state):
    after = snapshot(*request_events())
    if state is None:
        del after["traceEvents"][3]["args"]["capture_state"]
    else:
        after["traceEvents"][3]["args"]["capture_state"] = state
    row = measure(after)
    assert not row["trace_metadata_valid"]
    assert row["trace_capture_errors"]


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("events", [
    [span(10, "sql.query", 99, "active")],
    [span(10, "observation.command.process", 99, "active"), span(11, "sql.query", 10, "active")],
    [span(10, "observation.command.process", 11, "active"), span(11, "sql.query", 10, "active")],
    [span(10, "unknown.business", state="active"), span(11, "sql.query", 10, "active")],
    [span(10, "unknown.business"), span(11, "sql.query", 10, "active")],
    [span(10, "sql.unknown", state="active")],
    [span(10, "sql.query", 99)],
])
def test_history_benchmark_does_not_hide_unknown_or_broken_causal_chains(events):
    row = measure(snapshot(*request_events(), *deepcopy(events)))
    assert not row["trace_metadata_valid"]
    assert row["trace_capture_errors"] or row["unattributed_active_spans"]
    assert not row["active_unrelated_spans"]


@pytest.mark.e2e
@pytest.mark.proxy
def test_history_benchmark_does_not_copy_invalid_capture_state_into_report():
    after = snapshot(*request_events())
    private_state = "private-invalid-capture-state"
    after["traceEvents"][-1]["args"]["capture_state"] = private_state
    row = measure(after)
    assert not row["trace_metadata_valid"]
    assert private_state not in repr(row)


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("change", ["missing_root", "duplicate_root", "unattributed_history"])
def test_history_benchmark_preserves_root_and_history_acceptance(change):
    events = request_events()
    if change == "missing_root":
        events = events[1:]
    elif change == "duplicate_root":
        events.append(span(10, "proxy.inference_run"))
    else:
        events.append(span(10, "generation_chain.parent.resolve"))
    assert not measure(snapshot(*events))["request_span_valid"]


@pytest.mark.e2e
@pytest.mark.proxy
def test_history_benchmark_accepts_active_completion_without_counter_loss():
    active = span(10, "sql.query", state="active")
    before = snapshot(active)
    before["metadata"]["frozen_total"] = 2
    before["metadata"]["incomplete_total"] = 3
    after = snapshot(*request_events(), span(10, "sql.query"))
    after["metadata"]["frozen_total"] = 2
    after["metadata"]["incomplete_total"] = 2
    row = measure(after, before)
    assert row["trace_metadata_valid"]
    assert row["request_span_valid"]
    assert row["request_sql_span_count"] == 1
    assert row["active_unrelated_spans"] == []


@pytest.mark.e2e
@pytest.mark.proxy
@pytest.mark.parametrize("change", ["session_changed", "debug_disabled", "storage_error"])
def test_history_benchmark_rejects_reset_or_unreadable_recording(change):
    before, after = snapshot(), snapshot(*request_events())
    if change == "session_changed":
        after["metadata"]["debug_epoch"] += 1
    elif change == "debug_disabled":
        after["metadata"]["debug_enabled"] = False
    else:
        before["metadata"]["storage_status"] = "error"
        after["metadata"]["storage_status"] = "error"
        before["metadata"]["storage_errors"] = 1
        after["metadata"]["storage_errors"] = 1
    assert not measure(after, before)["trace_metadata_valid"]


@pytest.mark.e2e
@pytest.mark.proxy
def test_history_benchmark_accepts_successful_disk_spill_without_treating_it_as_loss():
    before, after = snapshot(), snapshot(*request_events())
    for field in ("spilled_completed", "spilled_sql_completed", "spilled_resource_samples"):
        before["metadata"][field] = 0
        after["metadata"][field] = 4
    row = measure(after, before)
    assert row["trace_metadata_valid"]
    assert row["request_span_valid"]
    assert row["request_sql_span_count"] == 1
