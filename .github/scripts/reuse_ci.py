"""按精确提交复用自动完整 CI；只输出经过确认的验证决策。"""

import json
import os
import sys
import time
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import Request, urlopen

CI_PATH = ".github/workflows/ci.yml"
ACTIVE_STATUSES = {"queued", "in_progress", "pending", "waiting", "requested"}


class GitHub:
    def __init__(self, api_url: str, repository: str, token: str):
        self.base_url = f"{api_url.rstrip('/')}/repos/{repository}"
        self.token = token

    def get(self, path: str):
        request = Request(
            f"{self.base_url}/{path}",
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {self.token}",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        with urlopen(request, timeout=30) as response:
            return json.load(response)

    def ci_runs(self, sha: str):
        page = 1
        while True:
            query = urlencode({"head_sha": sha, "event": "push", "per_page": 100, "page": page})
            runs = self.get(f"actions/workflows/ci.yml/runs?{query}")["workflow_runs"]
            yield from runs
            if len(runs) < 100:
                return
            page += 1

    def wait_for_success(self, run_id: int, sha: str):
        deadline = time.monotonic() + 110 * 60
        while True:
            run = self.get(f"actions/runs/{run_id}")
            if run["head_sha"] != sha:
                raise RuntimeError("Reusable CI no longer matches the requested commit")
            if run["status"] == "completed":
                if run["conclusion"] != "success":
                    raise RuntimeError(f"Reusable CI {run_id} finished with {run['conclusion']}")
                return run
            if run["status"] not in ACTIVE_STATUSES:
                raise RuntimeError(f"Reusable CI {run_id} has unexpected status {run['status']}")
            if time.monotonic() >= deadline:
                raise RuntimeError(f"Timed out waiting for reusable CI {run_id}")
            # 等待已知的上游验证完成，不重试失败运行，也不把未知状态当成通过。
            time.sleep(30)


def choose_run(runs, repository: str, sha: str, current_id: int, standalone: bool):
    candidates = [
        run
        for run in runs
        if run["id"] != current_id
        and (not standalone or run["id"] < current_id)
        and run["head_sha"] == sha
        and run["event"] == "push"
        and run["head_branch"] in {"main", "dev"}
        and run["path"] == CI_PATH
        and run["head_repository"]["full_name"].lower() == repository.lower()
        and (
            run["status"] in ACTIVE_STATUSES
            or (run["status"] == "completed" and run["conclusion"] == "success")
        )
    ]
    # 优先使用已确认的成功；独立 CI 只能等待更早的 CI，Release 不作为候选。
    return min(candidates, key=lambda run: (run["status"] != "completed", run["id"]), default=None)


def decide(env, github: GitHub):
    # PR 的合并提交和显式手动运行（含缓存预热）不复用自动分支检查。
    if env["GITHUB_EVENT_NAME"] != "push":
        return None
    ref = env["GITHUB_REF"]
    if ref not in {"refs/heads/main", "refs/heads/dev"} and not ref.startswith("refs/tags/v"):
        return None
    repository = env["GITHUB_REPOSITORY"]
    standalone = env["GITHUB_WORKFLOW_REF"].split("@", 1)[0] == f"{repository}/{CI_PATH}"
    candidate = choose_run(
        github.ci_runs(env["GITHUB_SHA"]),
        repository,
        env["GITHUB_SHA"],
        int(env["GITHUB_RUN_ID"]),
        standalone,
    )
    if candidate is None:
        return None
    print(f"Waiting for complete CI: {candidate['html_url']}", flush=True)
    return github.wait_for_success(candidate["id"], env["GITHUB_SHA"])


def main():
    env = os.environ
    github = GitHub(env["GITHUB_API_URL"], env["GITHUB_REPOSITORY"], env["GH_TOKEN"])
    reused = decide(env, github)
    with Path(env["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        output.write(f"run_checks={'true' if reused is None else 'false'}\n")
    message = "Running complete CI for this event." if reused is None else f"Reused successful complete CI: {reused['html_url']}"
    print(message)
    with Path(env["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as summary:
        summary.write(f"{message}\n")


if __name__ == "__main__":
    try:
        main()
    except (KeyError, OSError, ValueError, RuntimeError) as error:
        print(f"CI reuse failed: {error}", file=sys.stderr)
        sys.exit(1)
