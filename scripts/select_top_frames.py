#!/usr/bin/env python3
"""Select a capped, diverse, severity-prioritized subset of
find_ball_confidence_dips.py's exported frames across multiple runs
(camera/time-block combinations) for Label Studio review.

# Why this exists

find_ball_confidence_dips.py exports every frame near every confirmed
track-end or in-track confidence dip it finds - useful for completeness,
but on real footage this can be several hundred frames per 5-minute
block (e.g. one 5-minute RIGHT-camera block produced 759 frames across
212 events during 2026-09-15's testing). The user does not want to
review that many frames per match: "759 frames om te reviewen zit ik
echt niet op te wachten. 100 frames max per wedstrijd, daarom moeten we
goed kijken naar welke frames het meest belangrijk zijn voor training om
zo de confidence omhoog te krijgen."

# Selection strategy (user-confirmed 2026-09-15)

1. Severity: events with a lower min_conf are more informative - a
   confidence of 0.00 says the detector completely failed, 0.14 says it
   was barely below the production floor. Prioritize low min_conf.
2. event_type priority: `track_end` events (the ball was fully lost, not
   just briefly low-confidence) get priority over `in_track_dip` events,
   per the user's explicit choice ("Track-einde krijgt voorrang") - these
   are the clearest, most unambiguous training signal (see the frame
   3097/3098 example from this session's manual review, which the user
   called "perfect" for showing what training lacks).
3. Diversity via spatial clustering: events are clustered by
   (camera, approximate xy position) so a recurring spot (e.g. a ball
   repeatedly grazing the same field line) counts as ONE scenario, not
   one entry per occurrence - per the user's explicit choice ("Ruimtelijke
   clustering"). Only the single most severe event from each cluster
   competes for a slot; the rest of that cluster's occurrences are
   deliberately excluded even if individually severe, so the 100-frame
   budget isn't spent on near-duplicates of the same failure mode.
4. Events may be dropped entirely if they don't make the cut - no
   guaranteed per-track/per-event minimum ("Events mogen volledig
   wegvallen").

# What "spatial clustering" means here precisely

Two events on the SAME camera are one cluster if their xywhn centers are
within `--cluster-dist` (default 0.05, matching
build_ball_tracks's own --max-track-dist for the same "close enough to
be the same spot" intuition) of each other, using simple single-link
clustering (union-find) - NOT k-means or anything requiring a predecided
cluster count, since the number of distinct real-world trouble spots in
a match is unknown ahead of time. Events on different cameras are never
in the same cluster (a position on LEFT and the "same" xy on RIGHT are
different physical locations - each camera has its own frame).

# How this differs from select_ball_rich_frames.py

That script does its own fixed-interval extraction + teacher-model
scoring directly from raw footage to find frames RICH in ball
detections (a blind-spot-style sampler). This script instead operates
on find_ball_confidence_dips.py's ALREADY-COMPUTED tracks/events (no new
inference), across possibly several run directories at once, and
specifically targets frames where the ball was LOST or low-confidence,
capped and prioritized for a bounded review budget. Different signal,
different input, complementary tool - not a replacement.

Usage:
  python scripts/select_top_frames.py \\
      --run D:/CLAUDE/ball_dip_test_20260915/left \\
      --run D:/CLAUDE/ball_dip_test_20260915/right \\
      --run D:/CLAUDE/ball_dip_test_20260915/left_300_600 \\
      --run D:/CLAUDE/ball_dip_test_20260915/right_300_600 \\
      --max-frames 100 \\
      --out D:/CLAUDE/ball_dip_test_20260915/selected_100

NOT run as part of writing this script - point it at real
find_ball_confidence_dips.py output yourself (or ask me to).
"""

import argparse
import json
import shutil
import sys
from pathlib import Path


def load_detections(run_dir: Path) -> dict:
    """Load a run's cached raw detections (.cache/detections.json,
    filename -> list of {cls, conf, xywhn}) - used to verify an
    in_track_dip event's claimed min_conf is actually visible on one of
    the frames find_ball_confidence_dips.py itself exported, not just a
    property of a frame index that was never exported (see
    event_has_visible_evidence's doc comment for the real bug this
    fixes)."""
    path = run_dir / ".cache" / "detections.json"
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def event_exported_indices(event: dict, context_frames_per_event: int) -> list[int]:
    """The same span-sampling find_ball_confidence_dips.py's own export
    uses (see main()'s frame-writing loop) - the exact frame indices
    this selector would consider taking from this event's span, BEFORE
    checking which ones actually exist on disk."""
    span_start, span_end = event["span_start"], event["span_end"]
    indices = list(range(span_start, span_end + 1))
    if len(indices) > context_frames_per_event:
        step = (len(indices) - 1) / (context_frames_per_event - 1) \
            if context_frames_per_event > 1 else len(indices)
        indices = sorted({indices[round(i * step)] for i in range(context_frames_per_event)})
    return indices


def event_has_visible_evidence(event: dict, conf_threshold: float,
                                context_frames_per_event: int,
                                max_track_dist: float = 0.05) -> bool:
    """True if at least one of the frames this event would ACTUALLY
    export has a ball-class confidence at or below conf_threshold in the
    raw detections - i.e. the low confidence the event claims is visible
    on real, exportable evidence, not just true of some frame index in
    the middle of the span that find_ball_confidence_dips.py itself
    never kept.

    A real bug found 2026-09-15 by the user looking at an actual
    selected frame (Label Studio task #2307, `left_300_600_dip_f000396`,
    from an in_track_dip event claiming min_conf=0.0): the event's
    3-frame span [393, 395] had its lowest-confidence frame at index 394
    (never exported by find_ball_confidence_dips.py's own sampling -
    only 393 and 396, the span's neighbors, were kept). This selector
    picked 396, the only exportable frame near the claimed dip, which
    on inspection showed a perfectly clear, correctly-detected ball -
    "ik zie niet veel problemen mee". The event's min_conf was real (a
    genuine 0.0 existed somewhere in that span) but not VISIBLE on
    anything this selector could actually hand to a labeler - a
    training-relevant distinction find_ball_confidence_dips.py's own
    dip_summary.json doesn't capture (it records the span's overall
    min_conf, not which exported frame carries it).

    track_end events are NOT checked this way - deliberately: a
    track_end's whole point is the detector found NOTHING near the
    ball's last position (see build_ball_tracks), so there is no
    "ball-class confidence at the event's frame" to check in the same
    sense; the frame 3097/3098 example the user called "perfect" earlier
    this session is exactly this case and should not be filtered by a
    check designed for a different event type.

    A SECOND real bug, found immediately after re-running with the fix
    above and seeing task #2307 selected AGAIN unchanged: the first
    version of this check used `min(ball_confs)` over EVERY ball-class
    candidate on the frame, not just the one belonging to the actual
    tracked ball. Real footage always has a few near-zero-confidence
    ball-class false positives scattered across a frame (see
    find_ball_confidence_dips.py's own build_ball_tracks doc comment on
    this exact phenomenon) - so `min(ball_confs) <= 0.15` was true on
    almost every frame regardless of whether the TRACKED ball itself was
    ever low-confidence there, defeating the whole check. Fixed: only
    the ball-class candidate CLOSEST to the event's own `xywhn` (within
    `max_track_dist`, matching build_ball_tracks's own default) counts -
    the same "which candidate is the real tracked ball" logic that
    function already uses, applied here instead of a blind min() over
    every candidate on the frame."""
    if event["event_type"] != "in_track_dip":
        return True
    ref_xywhn = event.get("xywhn")
    if ref_xywhn is None:
        return True  # nothing to compare against - don't filter blind
    run = event["_run"]
    dets = run.get("_detections")
    if dets is None:
        dets = load_detections(run["_run_dir"])
        run["_detections"] = dets
    for idx in event_exported_indices(event, context_frames_per_event):
        fname = f"f{idx + 1:06d}.jpg"
        boxes = dets.get(fname)
        if boxes is None:
            continue
        balls = [b for b in boxes if b["cls"] == 1]
        near = [b for b in balls
                if ((b["xywhn"][0] - ref_xywhn[0]) ** 2
                    + (b["xywhn"][1] - ref_xywhn[1]) ** 2) ** 0.5 <= max_track_dist]
        if not near:
            # No ball-class candidate anywhere near the tracked position
            # on this exported frame - a genuine miss, same severity
            # class as a very low confidence.
            return True
        if min(b["conf"] for b in near) <= conf_threshold:
            return True
    return False


def load_run(run_dir: Path) -> dict:
    """Load one run's dip_summary.json, validating it has the
    event_type/min_conf/xywhn fields this script needs (added to
    find_ball_confidence_dips.py 2026-09-15 - an older dip_summary.json
    from before that change lacks them and must be regenerated via
    --reselect-only first, not silently degraded to guessed defaults
    here)."""
    summary_path = run_dir / "dip_summary.json"
    if not summary_path.exists():
        sys.exit(f"{run_dir}: no dip_summary.json found")
    summary = json.loads(summary_path.read_text(encoding="utf-8"))
    events = summary.get("events", [])
    if events and "event_type" not in events[0]:
        sys.exit(
            f"{summary_path}: events are missing event_type/min_conf/xywhn - "
            f"this is an OLD dip_summary.json from before find_ball_confidence_dips.py "
            f"was enriched to include them. Re-run that script with --reselect-only "
            f"against this run's --out to regenerate it (fast, no re-extraction/scoring)."
        )
    summary["_run_dir"] = run_dir
    return summary


def cluster_events(events: list[dict], cluster_dist: float) -> list[list[dict]]:
    """Grid-based clustering of events by xywhn center position - each
    event is bucketed into a `cluster_dist`-sized cell of a fixed 0..1
    normalized grid (floor(x/cluster_dist), floor(y/cluster_dist)), and
    events sharing a cell are one cluster.

    A real bug found and replaced 2026-09-15: an earlier version used
    single-link (union-find) clustering - two events merge if EITHER is
    within cluster_dist of the other, transitively. On real match data
    (the ball moves continuously across the pitch) this chains together
    almost everything: 341 LEFT-camera events collapsed into just 8
    "clusters", most of them spanning huge, physically meaningless
    swaths of the field, because a long chain of events each close to
    its neighbor eventually links opposite ends of the pitch. That
    defeated the whole point of clustering (treating a genuinely
    RECURRING spot as one scenario) - it instead merged unrelated
    scenarios that happened to be connected by intermediate points.

    Grid bucketing has no chaining: an event only ever clusters with
    another event in the exact same cell, regardless of what else is
    nearby. This slightly under-merges cases exactly on a cell boundary
    (two near-identical positions landing in adjacent cells stay
    separate) - a deliberate, bounded trade-off against the union-find
    version's unbounded over-merging, and consistent with how coarse the
    original --cluster-dist granularity already implies. Events with no
    xywhn (shouldn't happen for real find_ball_confidence_dips.py output,
    but defensive) each get their own singleton cluster."""
    cells: dict[tuple[int, int], list[dict]] = {}
    for i, e in enumerate(events):
        xy = e.get("xywhn")
        if xy is None:
            cells[("_no_xywhn", i)] = [e]
            continue
        cell = (int(xy[0] // cluster_dist), int(xy[1] // cluster_dist))
        cells.setdefault(cell, []).append(e)
    return list(cells.values())


def event_priority(event: dict) -> tuple:
    """Sort key for picking the winner within a cluster AND for the
    final cap ordering - lower sorts first (more important). event_type
    priority (track_end=0 beats in_track_dip=1) is the primary key per
    the user's explicit choice; min_conf (lower = more severe) breaks
    ties within the same type."""
    type_rank = 0 if event["event_type"] == "track_end" else 1
    return (type_rank, event["min_conf"])


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--run", type=Path, action="append", required=True,
                   help="A find_ball_confidence_dips.py --out directory "
                        "(must contain dip_summary.json, images/<camera>/, "
                        "labels/<camera>/). Repeat for multiple runs/blocks.")
    p.add_argument("--max-frames", type=int, default=100,
                   help="Hard cap on total frames in the output (default 100 - "
                        "the user's stated per-match review budget)")
    p.add_argument("--cluster-dist", type=float, default=0.05,
                   help="Max normalized xywhn distance for two events on the "
                        "SAME camera to count as the same recurring spot "
                        "(default 0.05, matching build_ball_tracks's own "
                        "--max-track-dist)")
    p.add_argument("--context-frames-per-event", type=int, default=3,
                   help="How many of an event's own exported frames to keep "
                        "(evenly sampled, endpoints included) - an event's "
                        "span in find_ball_confidence_dips.py's own export "
                        "can already be several frames; this caps how many "
                        "of THOSE make it into the final --max-frames budget "
                        "per selected event, so the winning events themselves "
                        "still get some temporal context instead of a single "
                        "frame each")
    p.add_argument("--out", type=Path, required=True, help="Output dataset dir")
    p.add_argument("--verify-conf-threshold", type=float, default=-1.0,
                   help="For in_track_dip events, require at least one of "
                        "the frames this script would ACTUALLY export to "
                        "have a ball-class confidence at or below this "
                        "value (or no ball-class candidate at all) in the "
                        "raw detections, restricted to the candidate "
                        "closest to the event's own tracked position (see "
                        "event_has_visible_evidence's doc comment). "
                        "DEFAULT DISABLED (-1, meaning 'accept every "
                        "event's own reported min_conf as-is') - this was "
                        "briefly added and defaulted on 2026-09-15 after "
                        "task #2307 (a low-confidence-but-visually-clear "
                        "ball) looked like a selection bug, but turned out "
                        "to be the assistant confusing that event's real "
                        "confidence (0.031, correctly low) with a "
                        "DIFFERENT frame's confidence (0.796) during "
                        "manual review. User explicitly confirmed such "
                        "cases SHOULD stay in the selection: 'elk laag-"
                        "confidence moment is waardevol' - a low model "
                        "score on a visually-clear ball is exactly the "
                        "kind of case training should improve, not "
                        "evidence of a tooling bug. Left available as an "
                        "opt-in (pass e.g. 0.15) for anyone who later "
                        "decides they DO want to filter down to only "
                        "visually-obvious difficulty cases - not the "
                        "default, since that was based on a mistaken "
                        "premise, not a confirmed requirement.")
    args = p.parse_args()

    runs = [load_run(d) for d in args.run]

    # Flatten all events across all runs, tagging each with which run/
    # camera it came from - clustering below is done PER CAMERA (see
    # module doc comment: a position on LEFT and the "same" xy on RIGHT
    # are different physical spots).
    all_events = []
    for run in runs:
        for e in run["events"]:
            all_events.append({**e, "_run": run})

    by_camera: dict[str, list[dict]] = {}
    for e in all_events:
        by_camera.setdefault(e["_run"]["camera"], []).append(e)

    # Pick the single best (highest-priority) event from each spatial
    # cluster, per camera - this is the diversity step: a recurring spot
    # only ever contributes its most severe occurrence.
    cluster_winners = []
    for camera, cam_events in by_camera.items():
        clusters = cluster_events(cam_events, args.cluster_dist)
        print(f"{camera}: {len(cam_events)} event(s) -> {len(clusters)} "
              f"spatial cluster(s) (--cluster-dist={args.cluster_dist})")
        for cluster in clusters:
            # Walk the cluster's candidates in priority order and take
            # the first one with VISIBLE evidence (see
            # event_has_visible_evidence's doc comment for the real bug
            # this prevents - task #2307, an in_track_dip whose claimed
            # min_conf belonged to a frame index find_ball_confidence_dips.py
            # never exported, so the only frame available to a labeler
            # looked perfectly fine: "ik zie niet veel problemen mee").
            # Falls back to the highest-priority candidate if NONE in
            # the cluster have visible evidence, rather than dropping
            # the cluster silently - still the best available signal for
            # that spot, just flagged as unverified in the log below.
            ranked = sorted(cluster, key=event_priority)
            winner = next(
                (e for e in ranked
                 if args.verify_conf_threshold < 0
                 or event_has_visible_evidence(e, args.verify_conf_threshold,
                                                args.context_frames_per_event,
                                                args.cluster_dist)),
                ranked[0],
            )
            cluster_winners.append(winner)

    # Final cap: sort ALL cluster winners (across all cameras/runs) by
    # priority and take events greedily until the frame budget would be
    # exceeded - the cap is on actual exported frames, matching what the
    # user will see in Label Studio, not on cluster count.
    cluster_winners.sort(key=event_priority)

    # Real bug found 2026-09-15: this used to write a FLAT images/ +
    # labels/ (with a "<camera>_" filename prefix to avoid collisions).
    # upload_to_labelstudio.py derives a frame's label path from its
    # image's PARENT DIRECTORY NAME (`img.parent.name`) - it assumes
    # find_ball_confidence_dips.py's own images/<camera>/labels/<camera>/
    # convention. Against a flat layout, `img.parent.name` resolved to
    # literally "images" for every file, so it looked for
    # labels/images/<stem>.txt (never exists) instead of
    # labels/<camera>/<stem>.txt. This wasn't caught until a REAL upload
    # to Label Studio: 90 of 91 images uploaded fine (task creation
    # doesn't touch labels at all), then the script crashed with
    # FileNotFoundError on the very first prediction-attach attempt -
    # leaving all 91 newly-created tasks in Label Studio with NO
    # bounding-box predictions at all. Fixed: output now uses
    # images/<camera>/ and labels/<camera>/ subdirectories, matching
    # find_ball_confidence_dips.py's own layout exactly, so
    # upload_to_labelstudio.py works unchanged. Filenames also now
    # include the source run's directory name (not just camera) to avoid
    # a real, if narrower, collision risk: two DIFFERENT time-block runs
    # for the same camera (e.g. "left" and "left_300_600") can both
    # produce a frame named f{idx:06d}.jpg for numerically-different but
    # coincidentally-equal idx values, which would otherwise silently
    # overwrite one selected frame with an unrelated one from a
    # different run.
    args.out.mkdir(parents=True, exist_ok=True)
    img_out = args.out / "images"
    lbl_out = args.out / "labels"
    if img_out.exists():
        shutil.rmtree(img_out)
    if lbl_out.exists():
        shutil.rmtree(lbl_out)
    img_out.mkdir(parents=True)
    lbl_out.mkdir(parents=True)

    classes_srcs = {run["_run_dir"] / "classes.txt" for run in runs}
    classes_content = (next(iter(classes_srcs))).read_text(encoding="utf-8")
    (args.out / "classes.txt").write_text(classes_content, encoding="utf-8")

    selected_log = []
    total_frames = 0
    for event in cluster_winners:
        if total_frames >= args.max_frames:
            break
        run = event["_run"]
        camera = run["camera"]
        run_dir = run["_run_dir"]
        img_dir = run_dir / "images" / camera
        lbl_dir = run_dir / "labels" / camera

        span_start, span_end = event["span_start"], event["span_end"]
        span_indices = list(range(span_start, span_end + 1))
        if len(span_indices) > args.context_frames_per_event:
            step = (len(span_indices) - 1) / (args.context_frames_per_event - 1) \
                if args.context_frames_per_event > 1 else len(span_indices)
            span_indices = sorted({span_indices[round(i * step)]
                                    for i in range(args.context_frames_per_event)})

        for idx in span_indices:
            if total_frames >= args.max_frames:
                break
            # find_ball_confidence_dips.py names frames f{idx+1:06d}.jpg
            # (ffmpeg's f%06d.jpg pattern starts at 1, not 0 - see that
            # script's own extract_frames/index convention).
            stem = f"dip_f{idx + 1:06d}"
            src_img = img_dir / f"{stem}.jpg"
            src_lbl = lbl_dir / f"{stem}.txt"
            if not src_img.exists():
                # This exact frame wasn't one find_ball_confidence_dips.py
                # itself kept (its own sampling can skip indices within a
                # span) - skip rather than fail the whole selection over
                # one missing frame.
                continue
            # dst_stem includes the source run dir's name (not just
            # camera) to avoid two different time-block runs of the same
            # camera silently colliding on the same f{idx:06d} stem -
            # see the fix note above cluster_winners.sort() for why.
            dst_stem = f"{run_dir.name}_{stem}"
            cam_img_out = img_out / camera
            cam_lbl_out = lbl_out / camera
            cam_img_out.mkdir(parents=True, exist_ok=True)
            cam_lbl_out.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src_img, cam_img_out / f"{dst_stem}.jpg")
            if src_lbl.exists():
                shutil.copy2(src_lbl, cam_lbl_out / f"{dst_stem}.txt")
            else:
                (cam_lbl_out / f"{dst_stem}.txt").write_text("", encoding="utf-8")
            # Carry the confidence sidecar along too (added to
            # find_ball_confidence_dips.py 2026-09-15) so
            # upload_to_labelstudio.py can show per-box confidence in
            # Label Studio - see that file's own doc comment for why
            # this is a separate file, not a 6th .txt column. Older runs
            # from before this existed simply have no .conf file, which
            # is fine - the upload step treats a missing .conf the same
            # as it always has (no score shown).
            src_conf = lbl_dir / f"{stem}.conf"
            if src_conf.exists():
                shutil.copy2(src_conf, cam_lbl_out / f"{dst_stem}.conf")
            total_frames += 1
            selected_log.append({
                "file": dst_stem, "camera": camera, "run": str(run_dir),
                "event_type": event["event_type"], "min_conf": event["min_conf"],
                "frame_idx": idx,
            })

    (args.out / "selection_summary.json").write_text(json.dumps({
        "max_frames": args.max_frames,
        "cluster_dist": args.cluster_dist,
        "runs": [str(d) for d in args.run],
        "total_clusters_considered": len(cluster_winners),
        "total_frames_selected": total_frames,
        "selected": selected_log,
    }, indent=2), encoding="utf-8")

    type_counts = {}
    for s in selected_log:
        type_counts[s["event_type"]] = type_counts.get(s["event_type"], 0) + 1
    print(f"\nSelected {total_frames} frame(s) from "
          f"{min(len(cluster_winners), args.max_frames)} of "
          f"{len(cluster_winners)} spatial cluster(s) across {len(runs)} run(s) "
          f"-> {args.out}")
    print(f"  event_type breakdown: {type_counts}")
    print("Reminder: same as find_ball_confidence_dips.py's own output - "
          "these are model predictions, not ground truth, verify/correct "
          "every one in Label Studio.")


if __name__ == "__main__":
    sys.exit(main())
