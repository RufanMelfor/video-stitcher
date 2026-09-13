# YOLO26 Training - Frame Selection & Upload

Quick-reference guide voor het selecteren van training frames uit new match footage en uploaden naar Label Studio.

## Voorbereiding

1. **Controleer dat Python beschikbaar is:**
   ```powershell
   python --version
   ```

2. **Zet Label Studio token klaar** - locatie waar je het hebt opgeslagen (e.g., `D:\Secrets\ls_token.txt`)

## Stap 1: Frame selectie uit match footage

Dit script extraheert frames waar het huidige model **fout gaat**:
- `uncertain_ball`: model vond bal maar twijfelt (lage confidence)
- `blind_spot`: model zag geen bal terwijl veel spelers op veld waren (model's echte misses)

**Commando template:**
```powershell
cd d:\VOETBAL_VIDEO\RECO\repository

python scripts/pick_training_frames.py `
    --match-dir "<MATCH_FOLDER>" `
    --model "<MODEL_CHECKPOINT>" `
    --out "<OUTPUT_DIR>" `
    --per-camera 15
```

**Voorbeeld voor "02 Berghem Sport - Zwaluw VFC" met merged_v1_tiled_1920:**
```powershell
python scripts/pick_training_frames.py `
    --match-dir "D:\VOETBAL_VIDEO\Berghem Sport J011-1\KNVB Competitie\02 Berghem Sport - Zwaluw VFC" `
    --model "D:\VOETBAL_VIDEO\RECO\training\merged_v1_tiled_1920\runs\full_patience100\weights\best.pt" `
    --out "D:\VOETBAL_VIDEO\RECO\training\round6_candidates" `
    --per-camera 15
```

**Output:**
- `D:\VOETBAL_VIDEO\RECO\training\round6_candidates/images/{left,right}/` - geëxtraheerde frames
- `D:\VOETBAL_VIDEO\RECO\training\round6_candidates/labels/{left,right}/` - YOLO labels (pre-labels van model)
- `D:\VOETBAL_VIDEO\RECO\training\round6_candidates/selection_summary.json` - statistieken

**Parameters:**
- `--per-camera N`: frames per camera (default 15)
- `--candidates M`: proofpunten om te proberen (default 180, trage eerste run)
- `--reselect-only`: herSelecteer uit cache zonder opnieuw te extracten/scoren (snel!)
- `--edge-skip SECONDS`: skip eerste/laatste N sec (default 90)
- `--ball-px MIN MAX`: plausibel balgrootte in pixels (default "5 30")

---

## Stap 2: Upload naar Label Studio

Dit stuurt beelden + pre-labels (als editable suggestions) naar een LS project.

**Commando template:**
```powershell
python scripts/upload_to_labelstudio.py `
    --dataset "<DATASET_DIR>" `
    --project <PROJECT_ID> `
    --url "http://<LS_HOST>:8080" `
    --token-file "<TOKEN_FILE_PATH>" `
    --model-version "<MODEL_VERSION_TAG>"
```

**Voorbeeld voor project 24 (Ai Learning - yolo26s):**
```powershell

& C:\Users\Rufan\AppData\Local\Programs\Python\Python314\python.exe scripts/upload_to_labelstudio.py `
    --dataset "D:\VOETBAL_VIDEO\RECO\training\round6_candidates" `
    --project 24 `
    --url "http://192.168.191.204:8080" `
    --token-file "D:\VOETBAL_VIDEO\RECO\training\token.txt" `
    --model-version "merged_v1_tiled_1920/full_patience100" `
    --set-project-model-version
    
```

**Output:**
- Tasks toegevoegd aan Label Studio project
- Pre-labels zichtbaar als editable suggestions
- User kan reviewen/corrigeren

**Parameters:**
- `--project ID`: Label Studio project nummer
- `--url HOST`: Label Studio URL (bijv. `http://192.168.1.100:8080`)
- `--token-file PATH`: bestand met LS API token (geheim, niet in git!)
- `--model-version TAG`: tag voor dit model (voor tracking), default = project's current setting
- `--set-project-model-version`: ook update project's model_version setting (opcional)

---

## Reference: Recent Models & Projects

**Huidige beste checkpoint:**
- `merged_v1_tiled_1920/runs/full_patience100/weights/best.pt`
  - Published op Hugging Face: https://huggingface.co/Dura-S/reco-yolo26s-football
  - Imgsz: 1920, Ball recall: ~61% op validation set

**Label Studio projects:**
- Project 8: "Finetuned yolo26n (rough v1)" - oorspronkelijke 200-image human-corrected set
- Project 24: "Ai Learning - yolo26s" - current active project (round 5+)

**Training folders:**
```
D:\VOETBAL_VIDEO\RECO\training\
├── merged_v1_tiled_1920/         <- current best checkpoint
├── round6_candidates/            <- volgende ronde input (deze stap maakt dit)
├── round6/                       <- output na training
└── ...
```

---

## Troubleshooting

**"Python not found":**
- Use full path: `C:\Users\Rufan\AppData\Local\Programs\Python\Python314\python.exe`

**"No video segments in {cam_dir}":**
- Check camera subdirs exist: `LEFT/` and `RIGHT/` (case-sensitive in script)
- Check .mp4/.MP4 files exist in those dirs

**Label Studio upload fails (401, 404):**
- Verify token file has correct access token
- Verify project ID is correct
- Verify LS URL is reachable (ping host)

**Pre-labels don't show up in LS:**
- Check `model-version` matches project's current setting
- Use `--set-project-model-version` flag to auto-update project setting

**Extraction is slow:**
- First run extracts frames from video (slow)
- Frames are cached -> re-select with different params for free (`--reselect-only`)
- Linear decode is ~1.4x realtime per video file

---

## Next Steps (after human review)

Once all new frames are reviewed/corrected in Label Studio:

1. Export from LS: `GET /api/projects/<id>/export?exportType=YOLO`
2. Run `prepare_yolo_train_split_from_ls_export.py` (existing script)
3. Run `yolo detect train` for new checkpoint - or
   `train_class_weighted.py` (see below) to also address the ball-
   class imbalance directly
4. Export to ONNX (`yolo export format=onnx imgsz=1920 nms=True`)
5. Test in real app: `reco stitch --events detections.jsonl`

See `docs/YOLO26_Training.md` for full training history & findings.

---

## Round 8+ (post-round-7): three tools added to break the "picky, not
## better" pattern, not yet used in an actual training round

Round 7 (see `docs/YOLO26_Training.md`'s round-7 entry) regressed real-
footage ball recall the SAME way the 2026-08 merged-data round did,
despite fixing both of that round's obvious causes (ball-size bias,
training from scratch). Two regressions with the same signature means
the real cause is still open. Three tools now exist to actually find
it, rather than guessing at another blind training variant:

**1. Is it a real capability loss or just a confidence-calibration
shift?** This was never actually measured - only a single-threshold
hit rate was ever compared. Run:

```powershell
python scripts/compare_checkpoints_real_footage.py `
    --left "<match>/LEFT/<file>.mp4" --right "<match>/RIGHT/<file>.mp4" `
    --old-checkpoint "training/merged_v1_tiled_1920/runs/full_patience100/weights/best.pt" `
    --new-checkpoint "training/round7_tiled_1920/runs/full_patience100/weights/best.pt" `
    --distribution
```

Read the "Interpretation" section it prints: if the new checkpoint is
behind the old one at EVERY confidence threshold, it's a real loss (a
data/training problem, not fixable by changing `--conf` in
production); if it only falls behind above some threshold, it's
recalibrated pickier, not less capable, and might not need retraining
at all - just running the new checkpoint at a lower `--conf`.

**2. Find frames where the WHOLE current model lineage fails, not
just one checkpoint's known blind spots.** `pick_training_frames.py`
now accepts `--model2`:

```powershell
python scripts/pick_training_frames.py `
    --match-dir "<new match footage not yet used for training>" `
    --model "training/merged_v1_tiled_1920/runs/full_patience100/weights/best.pt" `
    --model2 "training/round7_tiled_1920/runs/full_patience100/weights/best.pt" `
    --out "training/round8_candidates" --per-camera 15
```

`blind_spot` frames now require BOTH checkpoints to miss the ball
while players are on the pitch - a much stronger "the model lineage
genuinely doesn't see this kind of ball" signal than round 5's single-
teacher version, which is what round 7 trained on and still regressed.

**3. Actually address the class imbalance, not just curate around
it.** ~7900 person boxes against ~580 ball boxes (round 5 era) means
every added frame makes the ball relatively rarer even with perfect
frame selection. `train_class_weighted.py` sets a per-class loss
weight (ball weighted higher than person/referee) via Ultralytics'
existing but CLI-inaccessible `class_weights` loss hook - see that
script's own doc comment for exactly why a script is needed instead of
a `yolo detect train` flag, and its defaults for continuing from
round7 instead of training from scratch again.

**Whichever combination is tried next, ship nothing on val-mAP alone**
- run `compare_checkpoints_real_footage.py --distribution` against
the still-production `merged_v1_tiled_1920` checkpoint before
replacing it, exactly like every round so far.
