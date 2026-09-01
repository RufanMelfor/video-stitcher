#!/usr/bin/env python3
"""Patch every prediction in a Label Studio project to match the
project's own configured model_version - the fix for the "no labels
visible" symptom.

Label Studio only auto-displays, in the labeling UI, predictions whose
`model_version` matches the project's own `model_version` setting
(`GET /api/projects/<id>/` -> `"model_version"` - the same value the
Predictions/Model Version selector in the UI shows). A prediction
pushed with any other value renders with NO boxes, even though the
upload API returns 201 and total_predictions looks correct. This is
PER-PROJECT, not a fixed instance-wide value - e.g. on this Label
Studio instance project 8's setting is the literal string "undefined"
while project 24's is "yolo26s_v4_imgsz1920_prelabel" and project 16's
is unset (null). Don't assume a value; this script looks it up.

Hit repeatedly because it's easy to push predictions with a fresh,
descriptive model_version (e.g. a run name, for traceability) without
realizing the project has already settled on a different one from an
earlier batch - see upload_to_labelstudio.py's module doc and
project_yolo26n_training_pipeline.md (2026-08-12, 2026-08-13,
2026-08-29's round-5 push).

The task-list endpoint (`GET /api/tasks?project=...`) does NOT embed
predictions, only counts - this fetches each task individually
(`GET /api/tasks/<id>/`) to see actual per-prediction model_version
values, which is one HTTP round-trip per task (fine for the batch
sizes this project uses, ~100-200 tasks).

Usage:
  python3 fix_labelstudio_model_version.py --project 24 \\
      --url http://<host>:8080 --token-file /path/to/token.txt
"""

import argparse
import json
import sys
from urllib import error, request


def api(url, hdr, path, data=None, method=None):
    body = json.dumps(data).encode() if data is not None else None
    req = request.Request(f"{url}{path}", data=body,
                          method=method or ("POST" if body else "GET"))
    for k, v in hdr.items():
        req.add_header(k, v)
    if body:
        req.add_header("Content-Type", "application/json")
    with request.urlopen(req, timeout=120) as r:
        raw = r.read()
    return json.loads(raw) if raw else {}


def list_task_ids(url, hdr, project):
    ids, page = [], 1
    while True:
        try:
            d = api(url, hdr, f"/api/tasks?project={project}&page={page}&page_size=200")
        except error.HTTPError as e:
            if e.code == 404:  # LS 1.23.0 404s past the last page
                break
            raise
        rows = d.get("tasks", d if isinstance(d, list) else [])
        if not rows:
            break
        ids.extend(t["id"] for t in rows)
        page += 1
    return ids


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--project", type=int, required=True, help="Label Studio project id")
    p.add_argument("--url", required=True, help="Label Studio base URL")
    p.add_argument("--token-file", required=True, help="File holding the API token")
    p.add_argument("--target-version",
                   help="Value to patch every prediction to. Default: this project's "
                        "own current model_version setting (looked up automatically) - "
                        "only override this if you actually want to change what the "
                        "project shows, not just fix a mismatch.")
    p.add_argument("--dry-run", action="store_true", help="List what would change")
    args = p.parse_args()

    url = args.url.rstrip("/")
    with open(args.token_file) as f:
        token = f.read().strip()
    hdr = {"Authorization": f"Token {token}"}

    project = api(url, hdr, f"/api/projects/{args.project}/")
    target = args.target_version if args.target_version is not None else project.get("model_version")
    print(f"Project {args.project} \"{project.get('title')}\" - "
          f"target model_version: {target!r}"
          + ("" if args.target_version is None else " (explicit override)"), flush=True)
    if target in (None, ""):
        print("Project has no model_version set and none was given via --target-version - "
              "can't tell what value would make predictions visible. Aborting.", flush=True)
        return 1

    task_ids = list_task_ids(url, hdr, args.project)
    print(f"{len(task_ids)} tasks in project {args.project}", flush=True)

    to_fix = []
    for task_id in task_ids:
        task = api(url, hdr, f"/api/tasks/{task_id}/")
        for pred in task.get("predictions", []):
            if pred.get("model_version") != target:
                to_fix.append((task_id, pred["id"], pred.get("model_version")))

    print(f"{len(to_fix)} predictions need model_version -> {target!r}", flush=True)
    for task_id, pred_id, old in to_fix[:10]:
        print(f"  task {task_id} pred {pred_id}: {old!r} -> {target!r}")
    if len(to_fix) > 10:
        print(f"  ... and {len(to_fix) - 10} more")

    if args.dry_run or not to_fix:
        return 0

    fixed = 0
    for _task_id, pred_id, _old in to_fix:
        try:
            api(url, hdr, f"/api/predictions/{pred_id}/",
                {"model_version": target}, method="PATCH")
            fixed += 1
        except Exception as e:
            print(f"  pred {pred_id} FAILED: {e}", flush=True)

    print(f"\n{fixed}/{len(to_fix)} predictions patched", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
