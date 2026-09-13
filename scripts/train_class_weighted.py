#!/usr/bin/env python3
r"""Continue fine-tuning a YOLO checkpoint with per-class loss weights,
to directly counter the ball-class imbalance documented in
docs/YOLO26_Training.md: ~7900 person boxes against ~580 ball boxes as of
round 5 (144 ball / 1288 person / 119 referee for a single 100-task
sample), and every added frame historically contributes ~20 persons
but at most 1 ball - uniform sampling makes the ball *relatively*
rarer every round, which plain "add more frames" training can never
fix on its own.

# Why this needs a script, not a `yolo detect train ...` CLI flag

Ultralytics' v8DetectionLoss DOES support a per-class classification
loss multiplier (`bce_loss *= self.class_weights`, see
`ultralytics.utils.loss.v8DetectionLoss.__init__`/`__call__`) - but it
reads it from a `class_weights` attribute on the underlying
DetectionModel that nothing sets by default, and there is no
`yolo detect train class_weights=...` CLI argument for it. This script
sets that attribute via the `on_train_start` callback (fires after the
trainer builds its model, before the training loop begins) so the
existing CLI-driven workflow (`yolo detect train data=... model=...
imgsz=... epochs=... patience=...`) still works for everything else -
this only adds the one missing piece.

`cls` (classification loss GAIN, default 0.5) in `yolo detect train`
scales the WHOLE classification loss uniformly across every class -
it cannot make the model care more about ball specifically without
also making it care more about person/referee by the same factor.
Per-class weights are the correct lever for an imbalance problem;
`cls=` is not, despite looking related.

# Usage

Same required args as `yolo detect train`, plus --ball-weight (person/
referee stay at 1.0 - only ball needs boosting, they are not the
class that's starved):

  python scripts/train_class_weighted.py \
      --data training/round7_merged/data.yaml \
      --model training/round7_tiled_1920/runs/full_patience100/weights/best.pt \
      --imgsz 1920 --epochs 300 --patience 100 \
      --ball-weight 4.0 \
      --project training/round8_tiled_1920/runs --name full_patience100

--ball-weight is a starting point, not a tuned value - 4.0 approximates
the ~8:1 person:ball ratio without fully inverting it (a literal
inverse-frequency weight this extreme risks the opposite failure,
over-firing on anything vaguely ball-shaped). Compare the result
against the un-weighted checkpoint with
compare_checkpoints_real_footage.py --distribution before shipping,
exactly like every prior round - a val-mAP win here is not sufficient
either (see docs/YOLO26_Training.md's "Root cause found" entry: val-mAP
went up while real-footage recall went down, twice).

Class order is read from the model's own `names` (never hardcoded) -
see this project's repeated "don't assume COCO's/any fixed ordering"
convention (`reco_autocam::setup_autocam`'s `resolved_ball_class_id`,
this repo's own classes.txt-ordering bug documented in
docs/YOLO26_Training.md). If the checkpoint has no class named "ball",
this refuses to guess and exits instead of silently weighting the
wrong class.
"""

import argparse
import sys

import torch


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--data", required=True, help="Path to data.yaml")
    p.add_argument("--model", required=True, help="Checkpoint to continue fine-tuning from (.pt)")
    p.add_argument("--ball-weight", type=float, default=4.0,
                   help="Classification-loss multiplier for the ball class only "
                        "(person/referee stay at 1.0). Default 4.0 - see this "
                        "module's doc comment for why not a literal inverse-"
                        "frequency weight.")
    p.add_argument("--imgsz", type=int, default=1920)
    p.add_argument("--epochs", type=int, default=300)
    p.add_argument("--patience", type=int, default=100)
    p.add_argument("--batch", type=int, default=4)
    p.add_argument("--project", default=None, help="Ultralytics 'project' dir (runs land under <project>/<name>)")
    p.add_argument("--name", default="full_patience100")
    p.add_argument("--pretrained", action="store_true",
                   help="Re-init from COCO instead of continuing from --model's "
                        "trained weights. Off by default - round 7's whole point "
                        "was to stop training from scratch every round (see "
                        "docs/YOLO26_Training.md's round-7 entry); pass this only if "
                        "you deliberately want a from-scratch run.")
    args = p.parse_args()

    from ultralytics import YOLO

    model = YOLO(args.model)
    ball_id = None
    for idx, name in model.names.items():
        if name.lower() == "ball":
            ball_id = idx
            break
    if ball_id is None:
        sys.exit(f"{args.model}: no 'ball' class in model.names ({model.names}) - refusing to guess an index")
    nc = len(model.names)
    weights = torch.ones(nc)
    weights[ball_id] = args.ball_weight
    print(f"Class weights (nc={nc}, ball_id={ball_id}): {weights.tolist()}")

    def _apply_class_weights(trainer):
        # `trainer.model` is the real underlying DetectionModel the
        # loss function reads `class_weights` off of (see this module's
        # doc comment) - de-paralleled the same way v8DetectionLoss's
        # own __init__ expects ("model must be de-paralleled").
        trainer.model.class_weights = weights.to(trainer.device)
        print(f"[train_class_weighted] applied class_weights={weights.tolist()} "
              f"to trainer.model on {trainer.device}")

    model.add_callback("on_train_start", _apply_class_weights)

    model.train(
        data=args.data,
        imgsz=args.imgsz,
        epochs=args.epochs,
        patience=args.patience,
        batch=args.batch,
        pretrained=args.pretrained,
        project=args.project,
        name=args.name,
    )


if __name__ == "__main__":
    main()
