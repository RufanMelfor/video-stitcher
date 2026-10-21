# 6. Cuts, scoreboard and Match Logger

This chapter is about shaping the finished video: removing dead time, adding a
scoreboard, and making a goals-only reel. The **Match Logger** ties them
together: a small web page you use on a phone during the match to log what
happens, so the program can cut and draw the scoreboard for you afterwards.

You can use cuts without the Match Logger. The scoreboard and the automatic
features need it (or the live scoreboard editor).

## Cut ranges

A **cut range** removes a stretch of time from the export and joins what remains
with no gap. Use it for injuries, half-time, or a warm-up that was recorded.

Add a cut in either of two ways:

- **On the timeline:** drag the reddish band on the scrubber.
- **In the Export dialog:** click **+ Add cut** and type the start and end in
  seconds.

The audio follows the cuts, also across a recording made of several files.

The related **Processing range** (*Start* and *End* in the Export dialog) trims
the beginning and end of the whole export instead. It is a different thing: a
cut range removes something from the middle.

### The PAUZE overlay

If you switch on **PAUZE overlay on cuts**, every cut shows a short "PAUZE" dip
to black instead of an instant jump in the picture. Two settings control it:

- **Fade:** how long the dip to black and back takes. It is drawn over footage
  that was going to be encoded anyway, so it does not make the export longer.
- **Black:** how long the picture stays fully black. This is the only part that
  makes the export longer. Increase it if a pause should read as a real break.

With no cut ranges the overlay does nothing.

<!-- screenshot: cut ranges on the timeline -->

## The Match Logger

The Match Logger is **one web page** (`scripts/match-logger/Match Logger.html`).
It has no internet dependency, so you can keep it on your phone and open it in
the browser. It shows in English or Dutch according to the language of your
browser.

### Before kick-off

Fill in *Home team*, *Away team* and the *Number of periods*. Switch on *Track
yellow/red cards* if you want cards in the log.

### During the match

Tap, in this order:

1. **Start video** at the moment you press record on the cameras. This is the
   anchor the program uses to line the log up with the video, so try to tap it
   at the same moment.
2. **Start match (period 1)** at kick-off. Use **Next period** for the next one
   and **End match** at the final whistle.
3. **Goal** for the scoring team (and **Cards** if enabled) as they happen. You
   normally tap after the ball is in, which the program expects.
4. **Pause** when play is stopped for a longer time (for example an injury) and
   **Resume** when it restarts. Only logged pauses can be cut out automatically;
   the program will not guess at a break you forgot to mark.
5. Use **Added time** for stoppage time and **Add note** for anything else.

**Undo** removes the last event. **Download** saves the log as a small `.json`
file (it goes to your Downloads folder if sharing is not available on your
phone). **YouTube chapters** copies chapter marks (start, kick-off, periods,
goals, full time) you can paste into a video description. **New match** starts
over and clears all events, so download first.

### Getting the log into the program

Copy the `.json` file to the computer and put it **in the match folder**; the
program finds it by its content. Or, in the *Scoreboard* section, click **Load
Match Logger export...** and choose it. After loading you see a green summary of
the match, and the line *"Synced: video_start = 0.0 s into this video"*.

If the log has no *Start video* event, or the timing is off, scrub the video to
the matching frame and click **Set sync point**. Normally the anchor is the very
first frame of the video, so you only need this when you started the video later
than the log.

## The scoreboard

The scoreboard is an overlay drawn over the finished picture, after the
stitching and the AI camera, so it stays fixed while the camera moves. It is made
from a small web package, so it needs **Google Chrome or Chromium installed** on
the computer. If neither is found the program tells you so.

In the *Scoreboard* section (left panel, under *Rig alignment*):

1. Switch on **Overlay** and choose a scoreboard from the list.
2. Load the Match Logger export. The scoreboard then **replays the real events**:
   score, cards and clock follow the match as it was logged.
3. Without a log you can use **Edit scoreboard (live)...** to type the values by
   hand, and the **QR** code shows an address you can open on a phone to control
   the scoreboard from there.
4. **Drag the scoreboard on the preview** to move it. You can also set logos and
   the font in the editor.

Scoreboard settings are stored in the calibration file. Scoreboard packages live
in the `scoreboards` folder next to the program or in your `panopitch` data
folder; see [scoreboards/README.md](../../scoreboards/README.md) if you want to
make your own.

<!-- screenshot: scoreboard section and the scoreboard on the preview -->

## Automatic cuts from the log

With a Match Logger export loaded, these save you from marking cuts by hand:

- **Auto-cut lead-in + pauses:** adds cut ranges for the dead time before
  kick-off and for every pause you logged, keeping some seconds of context at
  each boundary. It does not touch cuts you made yourself. It only uses the
  *period start*, *pause* and *resume* events that are really in the log.
- **Pause cut** (*s before* / *s after*): how many seconds of play to keep around
  a cut-out pause. Keep a little before it so the incident that stopped play is
  still in the video, and a little after so the restart is not cut off.
- **Match trim** (*s pre-KO* / *s post-FT*): how many seconds to keep before
  kick-off and after the final whistle. These two move the start and end of the
  export, they are not cut ranges.

## Highlights only (goals)

Tick **Highlights only (goals)** to export just the goals: one clip per logged
goal, spliced together. It sets the export start, end and cuts for you and
changes the output name so it cannot overwrite your full-match export. Turning it
on turns the automatic cuts off, because it already leaves out everything that is
not a goal.

**Goal clip** sets how much of each goal to keep, in seconds before and after the
logged moment. The lead is longer than the trail on purpose: a goal is logged
*after* the ball goes in, so the build-up lies before the logged moment and only
the celebration comes after it.

Continue with [chapter 7: exporting](07-exporting.md).
