#!/usr/bin/env python3
"""Remove local filesystem paths from an Ultralytics model artifact.

Ultralytics records the training arguments inside everything it writes,
and those arguments hold the absolute paths the run happened to use.
A ``.pt`` checkpoint carries them twice - in the top-level
``train_args`` and again on the model object's own ``args``::

    data:     D:\\VOETBAL_VIDEO\\RECO\\training\\round4\\data.yaml
    project:  D:\\VOETBAL_VIDEO\\RECO\\training\\round4\\runs
    save_dir: D:\\VOETBAL_VIDEO\\RECO\\training\\round4\\runs\\<run name>

and an exported ``.onnx`` embeds one in its ``description`` metadata::

    Ultralytics YOLO26s model trained on D:\\...\\round4\\data.yaml

Harmless locally, but they ship with the model the moment it is
published, exposing the machine's directory layout and - on a default
install - the account name inside the home directory. Every path is
rewritten to its basename, which keeps the value readable ("data.yaml",
the run name) without the location.

Nothing else is touched: weights, class names, metrics, and the
Ultralytics license field all stay exactly as they were, so a cleaned
checkpoint is still a normal checkpoint to load, fine-tune, or export,
and a cleaned ONNX still carries the metadata a loader reads.

Usage::

    python scripts/strip_ultralytics_local_paths.py best.pt  best_clean.pt
    python scripts/strip_ultralytics_local_paths.py best.onnx best_clean.onnx
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

# A drive-letter path, a UNC share, or a POSIX home directory, matched
# anywhere in a string - ONNX buries one inside a sentence, so anchoring
# at the start would miss it. Matching on shape rather than on a list of
# known keys means a future ultralytics version storing a path under a
# new name is still caught; the verification pass relies on that.
LOCAL_PATH = re.compile(
    r"""(?:
          (?<![A-Za-z])[A-Za-z]:[\\/]  # C:\ or D:/ - the lookbehind keeps
                                       # "https://..." from matching on "s:/"
        | \\\\[^\\\s]+[\\/]            # \\server\share
        | /(?:home|Users)/             # /home/someone, /Users/someone
    )[^\s"';,]*""",
    re.VERBOSE,
)


def basename(path: str) -> str:
    """Last component of a path written in either separator style."""
    return pathlib.PurePath(path.replace("\\", "/")).name


def scrub_text(value: object) -> object:
    """Replace every path-like run inside a string with its basename."""
    if not isinstance(value, str) or not LOCAL_PATH.search(value):
        return value
    return LOCAL_PATH.sub(lambda m: basename(m.group(0)), value)


def scrub_mapping(args: object, label: str, report: list[str]) -> None:
    """Rewrite paths in one ultralytics args mapping, in place."""
    if args is None:
        return
    mapping = args if isinstance(args, dict) else getattr(args, "__dict__", None)
    if mapping is None:
        return
    for key, value in list(mapping.items()):
        cleaned = scrub_text(value)
        if cleaned != value:
            mapping[key] = cleaned
            report.append(f"  {label}.{key}: {value!r} -> {cleaned!r}")


def find_remaining(node: object, path: str = "") -> list[str]:
    """Every local-looking path still reachable from `node`.

    Walks dicts, lists, and plain objects' ``__dict__`` so a value that
    survives somewhere unexpected is reported rather than silently
    published. Tensors are skipped - a path cannot hide in one.
    """
    found: list[str] = []
    if isinstance(node, str):
        return [f"{path} = {node!r}"] if LOCAL_PATH.search(node) else []
    if isinstance(node, dict):
        for key, value in node.items():
            found += find_remaining(value, f"{path}.{key}" if path else str(key))
    elif isinstance(node, (list, tuple)):
        for i, value in enumerate(node):
            found += find_remaining(value, f"{path}[{i}]")
    elif hasattr(node, "__dict__") and not hasattr(node, "shape"):
        for key, value in vars(node).items():
            if not key.startswith("_"):
                found += find_remaining(value, f"{path}.{key}" if path else key)
    return found


def clean_checkpoint(src: pathlib.Path, dst: pathlib.Path) -> int:
    import torch

    checkpoint = torch.load(src, map_location="cpu", weights_only=False)

    report: list[str] = []
    scrub_mapping(checkpoint.get("train_args"), "train_args", report)
    for key in ("model", "ema"):
        module = checkpoint.get(key)
        if module is not None:
            scrub_mapping(getattr(module, "args", None), f"{key}.args", report)
    print_report(report)

    remaining = find_remaining(checkpoint)
    if remaining:
        return refuse(remaining)

    torch.save(checkpoint, dst)
    return written(dst)


def clean_onnx(src: pathlib.Path, dst: pathlib.Path) -> int:
    import onnx

    model = onnx.load(src)

    report: list[str] = []
    for prop in model.metadata_props:
        cleaned = scrub_text(prop.value)
        if cleaned != prop.value:
            report.append(f"  metadata.{prop.key}: {prop.value!r} -> {cleaned!r}")
            prop.value = cleaned
    if scrub_text(model.doc_string) != model.doc_string:
        report.append("  doc_string rewritten")
        model.doc_string = scrub_text(model.doc_string)
    print_report(report)

    remaining = [
        f"metadata.{p.key} = {p.value!r}"
        for p in model.metadata_props
        if LOCAL_PATH.search(p.value)
    ]
    if remaining:
        return refuse(remaining)

    onnx.save(model, dst)
    return written(dst)


def print_report(report: list[str]) -> None:
    if report:
        print(f"Rewrote {len(report)} path(s):")
        print("\n".join(report))
    else:
        print("No local paths found - nothing to rewrite.")


def refuse(remaining: list[str]) -> int:
    print("\nERROR: local paths still present, refusing to write:", file=sys.stderr)
    for item in remaining:
        print(f"  {item}", file=sys.stderr)
    return 1


def written(dst: pathlib.Path) -> int:
    print(f"\nWrote {dst} ({dst.stat().st_size / 1e6:.1f} MB)")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("input", type=pathlib.Path, help=".pt or .onnx to read")
    parser.add_argument("output", type=pathlib.Path, help="cleaned file to write")
    args = parser.parse_args()

    suffix = args.input.suffix.lower()
    if suffix == ".onnx":
        return clean_onnx(args.input, args.output)
    if suffix in (".pt", ".pth"):
        return clean_checkpoint(args.input, args.output)
    print(f"Unsupported input type {suffix!r} - expected .pt or .onnx", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
