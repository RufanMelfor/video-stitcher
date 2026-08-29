#!/usr/bin/env python3
"""Upload an `export_yolo_labels.py`-style dataset into an existing Label
Studio project, with the model's boxes attached as editable predictions.

Two Label Studio behaviours this works around, both hit for real:

1. The file-import API's response omits `task_ids` on LS 1.23.0, so newly
   created tasks are matched back by filename via `GET /api/tasks` instead
   of trusting the import response.
2. `GET /api/tasks` returns **404 past the last page** rather than an empty
   list, so the pagination loop treats 404 as "done" instead of crashing.

Pre-labels go in as *predictions*, not annotations, so a reviewer sees
editable suggestions rather than work that looks finished. Images are sent
through the import API (`data.image` becomes `/data/upload/<project>/...`),
which needs no local-files storage connection on the server - that route
additionally requires a registered storage row, not just the env vars.

The API token is read from a file so it never lands in a command line, a
shell history, or this repository. Create one in Label Studio under
Account & Settings -> Access Token.

Usage:
  python3 upload_to_labelstudio.py --dataset dataset_dir --project 24 \\
      --url http://<host>:8080 --token-file /path/to/token.txt \\
      --model-version "merged_v1_tiled_1920/full_patience100"
"""

import argparse
import json
import sys
from pathlib import Path
from urllib import error, request

CLASSES = ["person", "ball", "referee"]


class LabelStudio:
    def __init__(self, url: str, token: str, project: int):
        self.url = url.rstrip("/")
        self.hdr = {"Authorization": f"Token {token}"}
        self.project = project

    def api(self, path, data=None, method=None):
        body = json.dumps(data).encode() if data is not None else None
        req = request.Request(f"{self.url}{path}", data=body,
                              method=method or ("POST" if body else "GET"))
        for k, v in self.hdr.items():
            req.add_header(k, v)
        if body:
            req.add_header("Content-Type", "application/json")
        with request.urlopen(req, timeout=120) as r:
            raw = r.read()
        return json.loads(raw) if raw else {}

    def upload_image(self, path: Path):
        boundary = "----recoFrameUpload"
        head = (f"--{boundary}\r\n"
                f'Content-Disposition: form-data; name="file"; filename="{path.name}"\r\n'
                f"Content-Type: image/jpeg\r\n\r\n").encode()
        body = head + path.read_bytes() + f"\r\n--{boundary}--\r\n".encode()
        req = request.Request(f"{self.url}/api/projects/{self.project}/import",
                              data=body, method="POST")
        for k, v in self.hdr.items():
            req.add_header(k, v)
        req.add_header("Content-Type", f"multipart/form-data; boundary={boundary}")
        with request.urlopen(req, timeout=300) as r:
            return json.loads(r.read() or b"{}")

    def tasks_by_filename(self):
        """Original filename -> task id, for every task in the project.

        Uploaded images are stored as `<uuid>-<original name>`, so the uuid
        prefix is stripped back off here.
        """
        out, page = {}, 1
        while True:
            try:
                d = self.api(f"/api/tasks?project={self.project}"
                             f"&page={page}&page_size=200")
            except error.HTTPError as e:
                if e.code == 404:  # LS 404s past the last page
                    break
                raise
            rows = d.get("tasks", d if isinstance(d, list) else [])
            if not rows:
                break
            for t in rows:
                name = t.get("data", {}).get("image", "").rsplit("/", 1)[-1]
                if "-" in name:
                    name = name.split("-", 1)[1]
                out[name] = t["id"]
            page += 1
        return out


def to_result(boxes, width, height, from_name, to_name):
    """YOLO centre-xywh (normalised) -> Label Studio top-left xywh (percent)."""
    res = []
    for b in boxes:
        cx, cy, w, h = b["xywhn"]
        res.append({
            "from_name": from_name, "to_name": to_name, "type": "rectanglelabels",
            "original_width": width, "original_height": height, "image_rotation": 0,
            "value": {"x": max(0.0, (cx - w / 2) * 100), "y": max(0.0, (cy - h / 2) * 100),
                      "width": w * 100, "height": h * 100, "rotation": 0,
                      "rectanglelabels": [CLASSES[b["cls"]]]},
        })
    return res


def read_label(path: Path):
    boxes = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        cls, cx, cy, w, h = line.split()
        boxes.append({"cls": int(cls), "xywhn": [float(cx), float(cy), float(w), float(h)]})
    return boxes


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dataset", type=Path, required=True,
                   help="Dataset dir with images/<camera>/ and labels/<camera>/")
    p.add_argument("--project", type=int, required=True, help="Label Studio project id")
    p.add_argument("--url", required=True, help="Label Studio base URL")
    p.add_argument("--token-file", type=Path, required=True,
                   help="File holding the API token, so it stays out of the command line")
    p.add_argument("--model-version", default="teacher",
                   help="Shown in Label Studio as the source of the predictions")
    p.add_argument("--width", type=int, default=3840)
    p.add_argument("--height", type=int, default=2880)
    p.add_argument("--from-name", default="label", help="RectangleLabels control name")
    p.add_argument("--to-name", default="image", help="Image object name")
    p.add_argument("--limit", type=int, default=0, help="Only process N images (smoke test)")
    p.add_argument("--dry-run", action="store_true", help="List what would be sent")
    args = p.parse_args()

    ls = LabelStudio(args.url, args.token_file.read_text().strip(), args.project)
    images = sorted((args.dataset / "images").rglob("*.jpg"))
    if args.limit:
        images = images[:args.limit]

    known = ls.tasks_by_filename()
    todo = [i for i in images if i.name not in known]
    print(f"{len(images)} images, project holds {len(known)} tasks, "
          f"{len(todo)} to upload", flush=True)
    if args.dry_run:
        for i in todo:
            print("  would upload", i.name)
        return 0

    uploaded = []
    for i, img in enumerate(todo, 1):
        try:
            ls.upload_image(img)
            uploaded.append(img)
        except error.HTTPError as e:
            print(f"  {img.name} FAILED: {e.code} {e.read()[:200]}", flush=True)
        if i % 10 == 0:
            print(f"  uploaded {i}/{len(todo)}", flush=True)

    known = ls.tasks_by_filename()
    added, missing = 0, []
    for img in uploaded:
        task_id = known.get(img.name)
        if task_id is None:
            missing.append(img.name)
            continue
        boxes = read_label(args.dataset / "labels" / img.parent.name / f"{img.stem}.txt")
        ls.api("/api/predictions/", {
            "task": task_id, "model_version": args.model_version,
            "result": to_result(boxes, args.width, args.height,
                                args.from_name, args.to_name)})
        added += 1

    print(f"\n{len(uploaded)} tasks created, {added} with pre-labels", flush=True)
    if missing:
        print(f"no task matched for: {', '.join(missing)}", flush=True)
    print(f"project now holds {len(ls.tasks_by_filename())} tasks", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
