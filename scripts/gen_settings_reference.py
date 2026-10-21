#!/usr/bin/env python3
"""Generate docs/manual/settings-reference.md from the GUI's own help texts.

Every setting in `crates/reco-gui/ui/main.slint` carries a `tip:` string (the
text shown by the "?" badge). This script reads those tips together with the
widget's label, range and default value, so the manual's reference tables can
never drift away from what the app actually says.

The output is a generated file: edit the tips in `main.slint`, then re-run

    python scripts/gen_settings_reference.py

Use `--check` to fail (exit 1) when the committed file is out of date, which
is what CI or a pre-commit hook can run. Standard library only.
"""

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SLINT = REPO / "crates" / "reco-gui" / "ui" / "main.slint"
OUT = REPO / "docs" / "manual" / "settings-reference.md"

# A Slint string literal body (handles escaped quotes).
STR = r'"((?:[^"\\]|\\.)*)"'
TIP_RE = re.compile(r"\btip:\s*" + STR + r"\s*;")
LABEL_RE = re.compile(r"\b(?:label|text):\s*" + STR + r"\s*;")
TITLE_RE = re.compile(r"\btitle:\s*" + STR + r"\s*;")
MIN_RE = re.compile(r"\bminimum:\s*([^;]+);")
MAX_RE = re.compile(r"\bmaximum:\s*([^;]+);")
DEC_RE = re.compile(r"\bdecimals:\s*([^;]+);")
BIND_RE = re.compile(r"\b(?:value|checked|current-value|text|selected)\s*<=>\s*root\.([A-Za-z0-9-]+)")
PROP_RE = re.compile(r"property\s*<([^>]+)>\s*([A-Za-z0-9-]+)\s*:\s*([^;]+);")


def clean(text: str) -> str:
    """Make a Slint string safe for a Markdown table cell."""
    text = text.replace("\\n", " ").replace('\\"', '"').replace("\\\\", "\\")
    text = re.sub(r"\s+", " ", text).strip()
    return text.replace("|", "\\|")


def block_after(lines, i, limit):
    """Text of the `{ ... }` block that encloses line `i` (the tip's widget).

    The tip can come before or after the widget's `label:`, so the block is
    read from its opening brace, found by scanning upwards, to its closing one.
    """
    depth, first = 0, i
    for j in range(i - 1, max(-1, i - 40), -1):
        code = re.sub(STR, '""', lines[j])
        for ch in reversed(code):
            if ch == "}":
                depth += 1
            elif ch == "{":
                depth -= 1
        if depth < 0:
            first = j
            break
    depth, out = 0, []
    for line in lines[first : min(limit + 40, first + 80)]:
        code = re.sub(STR, '""', line)  # braces inside strings do not count
        for ch in code:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
        out.append(line)
        if depth <= 0 and any("{" in re.sub(STR, '""', l) for l in out):
            break
    return "\n".join(out)


def forward_block(lines, i, limit):
    """Text from line `i` to the end of the block it sits in (tip before label)."""
    depth, out = 0, []
    for line in lines[i : min(limit, i + 40)]:
        code = re.sub(STR, '""', line)
        for ch in code:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
        if depth < 0:
            break
        out.append(line)
    return "\n".join(out)


def header_title(lines, i):
    """Title of a `SectionHeader` whose `title:` may be an expression."""
    for line in lines[i : i + 6]:
        if "title:" in line or line.strip().startswith(("?", ":")):
            m = re.search(STR, line)
            if m:
                return m.group(1).rstrip(" -")
    return None


def pretty(name: str) -> str:
    words = name.title().split()
    fixed = {"Ai": "AI", "Roi": "ROI", "Imu": "IMU", "Akaze": "AKAZE"}
    return " ".join(fixed.get(w, w) for w in words)


def collect(src: str):
    lines = src.split("\n")
    start = next(i for i, l in enumerate(lines) if "export component RecoApp" in l)
    end = next((i for i, l in enumerate(lines) if "export component DebugWindow" in l), len(lines))

    defaults = {m.group(2): (m.group(1).strip(), m.group(3).strip()) for m in PROP_RE.finditer(src)}

    def find(pattern: str) -> int:
        """Line index of the first line (inside RecoApp) matching `pattern`."""
        return next((i for i in range(start, end) if re.search(pattern, lines[i])), end)

    # Dialog boundaries, found by their markers so the script survives edits.
    prefs_at = find(r"if root\.prefs-dialog-open:")
    export_at = find(r"if root\.export-dialog-open:")
    export_tab_at = find(r'export-dialog-tab == "export":')
    ai_tracking_at = find(r'text:\s*"AI Tracking";')
    ai_debug_at = find(r'export-dialog-tab == "ai-debug":')
    panel_end_at = prefs_at  # the right-hand panel ends where the dialogs begin
    pref_heading = re.compile(r'Text \{ text: "([^"]+)";[^}]*font-weight: 600')

    # Section = last SectionHeader title, group = last other upper-case title.
    tip_lines = [i for i in range(start, end) if TIP_RE.search(lines[i])]
    entries = []
    section_at = {}
    section, group = "Stitching panel", ""
    for i in range(start, end):
        t = TITLE_RE.search(lines[i])
        is_header = bool(t) and "SectionHeader" in "".join(lines[max(0, i - 3) : i + 1])
        if ":= SectionHeader" in lines[i] and header_title(lines, i) and not (t and is_header):
            t = re.search(STR, header_title(lines, i) and '"' + header_title(lines, i) + '"')
            is_header = True
        if i == prefs_at:
            section, group = "Preferences", ""
        elif i == export_at:
            section, group = "Export dialog", "Export tab"
        elif i == export_tab_at:
            group = "Export tab"
        elif i == ai_tracking_at:
            group = "AI tracking"
        elif i == ai_debug_at:
            group = "AI Debug tab"
        elif section == "Preferences":
            h = pref_heading.search(lines[i])
            if h:
                group = h.group(1)
        elif t and is_header:
            if section == "Export dialog":
                group = t.group(1)  # e.g. "Advanced panner" inside AI tracking
            else:
                section, group = t.group(1), ""
        elif t and t.group(1).isupper() and len(t.group(1)) > 3 and section != "Export dialog":
            group = t.group(1)
        section_at[i] = (section, group)

    for n, i in enumerate(tip_lines):
        nxt = tip_lines[n + 1] if n + 1 < len(tip_lines) else end
        tip = TIP_RE.search(lines[i]).group(1)
        # The tip sits inside the widget (or `Tip { ... }` wrapper) it
        # describes, so only read up to the end of that enclosing block.
        after = block_after(lines, i, limit=nxt)
        if len(TIP_RE.findall(after)) > 1:
            # Several tips share one parent block (e.g. the lens grid): the
            # label then follows this tip instead of preceding it.
            after = forward_block(lines, i, limit=nxt)
        label = LABEL_RE.search(after)
        mn, mx, dec = (r.search(after) for r in (MIN_RE, MAX_RE, DEC_RE))
        bind = BIND_RE.search(after)
        if label is None and bind is None:
            continue  # informational text, not a setting
        default = defaults.get(bind.group(1)) if bind else None
        sec, grp = section_at[i]
        entries.append(
            {
                "section": sec,
                "group": grp,
                "label": clean(label.group(1)) if label else (bind.group(1).replace("-", " ") if bind else "(unlabelled)"),
                "tip": clean(tip),
                "min": mn.group(1).strip() if mn else None,
                "max": mx.group(1).strip() if mx else None,
                "default": default[1] if default else None,
                "type": default[0] if default else None,
            }
        )
    return entries


def fmt_default(entry) -> str:
    d = entry["default"]
    if d is None or entry["group"] == "LENS PARAMETERS":
        return ""  # lens values are placeholders, filled from the loaded calibration
    if entry["type"] == "bool":
        return "on" if d == "true" else "off"
    return d.strip('"') or "(empty)"


def render(entries) -> str:
    out = [
        "# Settings reference",
        "",
        "<!-- GENERATED by scripts/gen_settings_reference.py from the help texts in",
        "     crates/reco-gui/ui/main.slint. Do not edit by hand: change the tip in the",
        "     Slint file and re-run the script. -->",
        "",
        "Every setting below is shown in the app with a **?** badge that displays the",
        "same text. Defaults are the values the app starts with before any saved",
        "calibration or preference overrides them. A blank default means the value is",
        "filled in by the app (for example from the loaded calibration). Picking a",
        "**Style preset** in the AI Tracking section overwrites the framing sliders,",
        "so their defaults only apply until you pick one.",
        "",
    ]
    by_section = {}
    for e in entries:
        by_section.setdefault(e["section"], []).append(e)
    for section, items in by_section.items():
        out += [f"## {pretty(section) if section.isupper() else section}", ""]
        group = None
        rows = []

        def flush():
            if rows:
                out.extend(
                    ["| Setting | Range | Default | What it does |", "|---|---|---|---|"] + rows + [""]
                )
                rows.clear()

        seen = set()
        for e in items:
            key = (e["group"], e["label"], e["tip"])
            if key in seen:  # e.g. the lens table repeats per camera
                continue
            seen.add(key)
            if e["group"] != group:
                flush()
                group = e["group"]
                if group:
                    out += [f"### {pretty(group) if group.isupper() else group}", ""]
            rng = f"{e['min']} to {e['max']}" if e["min"] is not None and e["max"] is not None and "root." not in f"{e['min']}{e['max']}" else ""
            rows.append(f"| {e['label']} | {rng} | {fmt_default(e)} | {e['tip']} |")
        flush()
    return "\n".join(out).rstrip() + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="exit 1 if the generated file is out of date")
    args = ap.parse_args()

    entries = collect(SLINT.read_text(encoding="utf-8"))
    text = render(entries)
    if args.check:
        current = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        if current.replace("\r\n", "\n") != text:
            print(f"{OUT} is out of date, run scripts/gen_settings_reference.py", file=sys.stderr)
            return 1
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"{len(entries)} settings written to {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
