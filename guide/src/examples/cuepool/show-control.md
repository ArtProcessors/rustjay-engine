# Show Control

CuePool can be driven by OSC, MIDI, MIDI Show Control, wall-clock time, and
its own show clock — and can drive other machines running CuePool as remote
nodes.

## OSC

*Project Settings → OSC / Remote* sets the network interface and ports
(defaults: receive 9000, transmit 8000).

Incoming commands (the address namespace is kept as `/qplayer` for
compatibility with the original QPlayer remote protocol and existing
controller rigs):

| Address | Arguments | Action |
|---|---|---|
| `/qplayer/go` | optional cue # | GO — or fire a specific cue |
| `/qplayer/stop` | optional cue # | Stop all — or one cue |
| `/qplayer/pause` | optional cue # | Pause all — or one cue |
| `/qplayer/unpause` | optional cue # | Resume all — or one cue |
| `/qplayer/preload` | cue #, time | Decode a cue and hold it Ready |
| `/qplayer/select` | cue # | Move the selection |
| `/qplayer/up` / `/qplayer/down` | — | Move the selection |
| `/qplayer/save` | — | Save the project |

Outbound messages are sent by [OSC cues](cues.md#osc) — command format
`/address,arg1,arg2,…`.

### DMX recorder

The [DMX Recorder](lighting.md#dmx-recorder) listens on the same OSC port:

| Address | Arguments | Action |
|---|---|---|
| `/dmx/{universe}/{channel}` | float 0–1 (or int 0–255) | Set a DMX channel (1-based) — live bridge, recorded while a pass runs |
| `/recorder/record` | — | Start a pass on the selected take; again = stop & keep |
| `/recorder/stop` | — | Stop the pass (keep) — or stop preview when idle |
| `/recorder/play` | — | Preview the selected take on the lighting output |
| `/recorder/select` | take name/path | Choose the target take (`.dmxrec` appended if missing) |
| `/recorder/discard` | — | Throw the in-flight pass away |
| `/recorder/revert` | — | Swap the take with its previous version |

Build a touchOSC layout of faders addressed `/dmx/1/1`, `/dmx/1/2`, … and
you have a hand-held DMX console; values are held (latest wins) until
**Clear** in the recorder panel. MIDI CC works the same way: enable
**MIDI CC → universe** in the panel and CC# = channel. Status feedback
(recording LEDs etc.) is not implemented yet.

A ready-made layout ships at
`examples/cuepool/assets/CuePool-DMX-Recorder.touchosc` — 16 faders
(universe 1, ch 1–16, two pages) plus REC / STOP / PLAY / DISCARD / REVERT.
It's the classic `.touchosc` format: open it directly in TouchOSC Mk1, or
**File → Import** in current TouchOSC. Point the connection's *send* host
at the CuePool machine on the OSC RX port from Settings. Buttons fire on
press only — the release value of 0 is ignored.

## Per-cue triggers

Every cue has an optional *Triggers* section in the Inspector, in addition
to the GO chain:

- **Hotkey** — a key that fires the cue directly.
- **MIDI** — Note On / Note Off / CC on a channel, with a minimum velocity.
- **Wall clock** — a time of day (12/24-hour), once or daily. Useful for
  house music, pre-show loops, and installations.
- **Timecode** — a time on the show clock, which is started by a
  [TimeCode cue](cues.md#timecode). A capture button stamps the trigger with
  the current show time.

## MIDI Show Control (MSC)

*Project Settings → MSC* enables MSC over the network (default ports 6004,
receive device `0x70`, transmit device `0x71`, optional executor/page
filters), so a lighting desk can GO CuePool — or vice versa.

## Remote nodes

*Project Settings → OSC / Remote* also enables **remote control**: multiple
CuePool machines discover each other over OSC by node name. One node is the
host; the others are clients. With *sync show file on save* enabled, saving
on the host pushes the project to the clients. Set a cue's *Remote Node*
field to make it fire on that named machine instead of locally — e.g. a
video machine at front of house triggered from the sound desk.
