# 4. Colour and sharpness

Two cameras never see the scene identically. One may be a little brighter, have
a slightly different white balance, or reproduce contrast differently. The
program can correct this so the two halves of the panorama look like one picture.
All settings in this chapter are in the **Color Mapping** section of the right
panel and most of them **update live**, so you can watch the effect on the
preview while you drag a slider.

<!-- screenshot: Color Mapping panel -->

## How to work, in order

1. **Leave Auto color match on.** It handles most matches by itself.
2. **If the halves still differ in contrast** (one looks flatter or punchier),
   use **Gamma left** and **Gamma right**.
3. **If sky and grass cannot both be made to match**, try **Overlap fit**.
4. **Only at the end**, if the footage looks dull or soft, use **Vivid** and
   **Sharpening**.

Always judge the result on the **seam**: it should be hard to say where one
camera ends and the other begins.

## Auto color match

On by default. The program periodically measures the colour of each camera in a
narrow band next to the seam and nudges both toward their shared average, so an
exposure or white-balance difference does not show up as a colour step. It is
independent of the geometric alignment and works in the preview and in every
export.

It shifts the **average** brightness and colour only. It cannot reshape the
contrast curve. For a contrast difference use the manual gamma below.

### Measurement

These control how the measurement is taken. The defaults are good; change them
only for a reason.

| Setting | Meaning |
|---|---|
| **MEAS Band Width** | How wide a strip next to the seam is sampled (0.02 to 1.0, starts at 0.15). Independent of the seam blend width. |
| **Measure interval** | Re-measure every N rendered frames (1 to 120, starts at 15). |
| **Grid columns / Grid rows** | How many sample points per camera. More points give a steadier measurement at a small extra cost. |

### Correction limits

These keep the automatic correction from going too far.

| Setting | Meaning |
|---|---|
| **Max Y offset** | The most brightness correction either camera can receive (starts at 0.06). |
| **Max chroma offset** | The most colour correction either camera can receive (starts at 0.04). |
| **Smoothing (EMA alpha)** | How fast the correction follows a new measurement. Higher is faster but noisier (starts at 0.15). |

**Remeasure now** forces a fresh measurement against the current frame at once.
Use it while paused, or right after you jump to a part of the match with
different light, instead of waiting for the next automatic measurement.

## Manual gamma

*Gamma left* and *Gamma right* (0.5 to 2.0, both start at 1.0) reshape one
camera's mid-tones before the automatic match measures anything. Above 1.0 lifts
the mid-tones, below 1.0 lowers them. Use this when the cameras differ in
**contrast** rather than overall brightness. Set the two against each other
until both halves match in the mid-tones, then let Auto color match take care of
the rest. The values apply in the preview and in every export.

**Auto gamma (dynamic)** re-fits the two gamma values for you during the match,
from the same seam measurement. It is useful when the light changes during a
match, because a fixed value that fixes a large mismatch at one moment can
overshoot later. While it is on, the gamma sliders still move but your values are
overwritten at the next measurement. **Turn Auto gamma off before setting a fixed
value** that has to stay.

## Overlap fit (experimental)

A different way of matching that measures both cameras at exactly the same spots
next to the seam, from the sky down to the near touchline, and fits a brightness
and contrast correction per colour channel. That lets sky and grass both match,
which the classic method cannot do because it compares a different piece of the
field for each camera. While it is on, the band width, grid and max offset
settings and Auto gamma are not used. It is off by default because it is new;
try it when the classic method leaves a visible difference in the sky.

## Vivid colour grade

A single switch that perks up grey, dull footage. Switching it on sets
brightness, saturation and gamma to a tuned preset; the three sliders below it
let you fine-tune (1.0 means *no change*, range 0.5 to 1.5). You can also leave
the switch off and set the sliders directly. It is stored in the setup and
costs no GPU time when off.

## Sharpening

*Sharpen amount* (0 to 2, starts at 0 = off) applies unsharp-mask sharpening to
the final picture. It compensates for the loss of detail when the AI camera zooms
into a crop of the panorama. **Radius** is how coarse the sharpened detail is:
larger helps when zoomed in heavily, smaller targets fine detail. Above about 1.5
you start to see bright halos around high-contrast edges.

## A note on what you see

The preview and the export use the same colour processing. If the preview looks
right but the export does not, see [chapter 8](08-troubleshooting.md).

Continue with [chapter 5: AI tracking](05-ai-tracking.md).
