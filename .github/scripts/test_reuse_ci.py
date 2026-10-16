import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from reuse_ci import GitHub

SCRIPT = Path(__file__).with_name("reuse_ci.py")
REPOSITORY = "example/project"
SHA = "a" * 40


def ci_run(run_id, **changes):
    run = {
        "id": run_id,
        "head_sha": SHA,
        "event": "push",
        "head_branch": "dev",
        "path": ".github/workflows/ci.yml",
        "head_repository": {"full_name": REPOSITORY},
        "status": "in_progress",
        "conclusion": None,
        "html_url": f"https://github.com/{REPOSITORY}/actions/runs/{run_id}",
    }
    run.update(changes)
    return run


@contextmanager
def api_fixture(pages, results=None, status=200):
    results = results or {}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            url = urlsplit(self.path)
            if url.path.endswith("/actions/workflows/ci.yml/runs"):
                page = int(parse_qs(url.query)["page"][0])
                payload = {"workflow_runs": pages.get(page, [])}
            else:
                run_id = int(url.path.rsplit("/", 1)[1])
                sequence = results[run_id]
                payload = sequence.pop(0) if len(sequence) > 1 else sequence[0]
            body = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


class ReuseCITests(unittest.TestCase):
    def run_gate(self, api_url, **changes):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory, "output")
            summary = Path(directory, "summary")
            env = {
                **os.environ,
                "GITHUB_API_URL": api_url,
                "GITHUB_REPOSITORY": REPOSITORY,
                "GH_TOKEN": "fixture-token",
                "GITHUB_SHA": SHA,
                "GITHUB_RUN_ID": "11",
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_REF": "refs/heads/main",
                "GITHUB_WORKFLOW_REF": f"{REPOSITORY}/.github/workflows/ci.yml@refs/heads/main",
                "GITHUB_OUTPUT": str(output),
                "GITHUB_STEP_SUMMARY": str(summary),
                **changes,
            }
            result = subprocess.run(
                [sys.executable, str(SCRIPT)], env=env, text=True, capture_output=True, check=False
            )
            return result, output.read_text() if output.exists() else ""

    def test_dev_main_and_release_schedule_one_complete_suite(self):
        runs = [ci_run(11, head_branch="main"), ci_run(10)]
        with api_fixture({1: runs}, {10: [ci_run(10, status="completed", conclusion="success")]}) as url:
            decisions = []
            for changes in [
                {"GITHUB_RUN_ID": "10", "GITHUB_REF": "refs/heads/dev"},
                {},
                {
                    "GITHUB_RUN_ID": "12",
                    "GITHUB_REF": "refs/tags/v1.2.3",
                    "GITHUB_WORKFLOW_REF": f"{REPOSITORY}/.github/workflows/release.yml@refs/tags/v1.2.3",
                },
            ]:
                result, output = self.run_gate(url, **changes)
                self.assertEqual(result.returncode, 0, result.stderr)
                decisions.append(output)
            self.assertEqual(decisions, ["run_checks=true\n", "run_checks=false\n", "run_checks=false\n"])

    def test_ineligible_evidence_cannot_skip_checks(self):
        cases = [
            {"head_sha": "b" * 40},
            {"event": "pull_request"},
            {"event": "workflow_dispatch"},
            {"head_branch": "feature"},
            {"head_repository": {"full_name": "fork/project"}},
            {"path": ".github/workflows/release.yml"},
            *[{"status": "completed", "conclusion": value} for value in ["failure", "cancelled", "skipped", "neutral", "timed_out"]],
        ]
        for changes in cases:
            with self.subTest(changes=changes):
                with api_fixture({1: [ci_run(10, **changes)]}) as url:
                    result, output = self.run_gate(url)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output, "run_checks=true\n")

    def test_pr_and_explicit_manual_runs_always_validate_themselves(self):
        with api_fixture({1: [ci_run(10, status="completed", conclusion="success")]}) as url:
            for event in ["pull_request", "workflow_dispatch"]:
                with self.subTest(event=event):
                    result, output = self.run_gate(url, GITHUB_EVENT_NAME=event)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(output, "run_checks=true\n")

    def test_standalone_run_does_not_wait_for_a_newer_run(self):
        with api_fixture({1: [ci_run(12)]}) as url:
            result, output = self.run_gate(url)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "run_checks=true\n")

    def test_confirmed_success_takes_precedence_over_an_unfinished_run(self):
        successful = ci_run(11, status="completed", conclusion="success")
        with api_fixture(
            {1: [ci_run(10), successful]},
            {10: [ci_run(10, status="completed", conclusion="failure")], 11: [successful]},
        ) as url:
            result, output = self.run_gate(url, GITHUB_RUN_ID="20")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "run_checks=false\n")

    def test_release_can_wait_for_ci_even_if_events_were_registered_out_of_order(self):
        with api_fixture({1: [ci_run(12)]}, {12: [ci_run(12, status="completed", conclusion="success")]}) as url:
            result, output = self.run_gate(
                url,
                GITHUB_REF="refs/tags/v1.2.3",
                GITHUB_WORKFLOW_REF=f"{REPOSITORY}/.github/workflows/release.yml@refs/tags/v1.2.3",
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "run_checks=false\n")

    def test_pagination_finds_the_original_ci_owner(self):
        newer = [ci_run(run_id) for run_id in range(200, 300)]
        with api_fixture({1: newer, 2: [ci_run(10)]}, {10: [ci_run(10, status="completed", conclusion="success")]}) as url:
            result, output = self.run_gate(url)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "run_checks=false\n")

    def test_failed_or_cancelled_dependency_fails_the_gate_without_a_skip_decision(self):
        for conclusion in ["failure", "cancelled", "timed_out", "skipped"]:
            with self.subTest(conclusion=conclusion):
                with api_fixture({1: [ci_run(10)]}, {10: [ci_run(10, status="completed", conclusion=conclusion)]}) as url:
                    result, output = self.run_gate(url)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(conclusion, result.stderr)
                self.assertEqual(output, "")

    def test_api_errors_do_not_turn_into_successful_reuse(self):
        with api_fixture({1: []}, status=403) as url:
            result, output = self.run_gate(url)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("403", result.stderr)
        self.assertEqual(output, "")

    def test_wait_requires_success_after_active_states(self):
        sequence = [ci_run(10, status="queued"), ci_run(10), ci_run(10, status="completed", conclusion="success")]
        with api_fixture({}, {10: sequence}) as url, patch("reuse_ci.time.sleep"):
            run = GitHub(url, REPOSITORY, "fixture-token").wait_for_success(10, SHA)
        self.assertEqual(run["conclusion"], "success")

    def test_wait_rejects_a_different_commit(self):
        with api_fixture({}, {10: [ci_run(10, head_sha="b" * 40, status="completed", conclusion="success")]}) as url:
            with self.assertRaisesRegex(RuntimeError, "commit"):
                GitHub(url, REPOSITORY, "fixture-token").wait_for_success(10, SHA)

    def test_wait_timeout_cannot_skip_checks(self):
        with api_fixture({}, {10: [ci_run(10)]}) as url, patch("reuse_ci.time.monotonic", side_effect=[0, 110 * 60]):
            with self.assertRaisesRegex(RuntimeError, "Timed out"):
                GitHub(url, REPOSITORY, "fixture-token").wait_for_success(10, SHA)


if __name__ == "__main__":
    unittest.main()
