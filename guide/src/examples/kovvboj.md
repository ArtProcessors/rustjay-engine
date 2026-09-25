# KOVVBOJ — Multi-Layer VJ App

KOVVBOJ, the flagship app built on this engine, lives in its own repository:
**[kovvbojAV/kovvboj](https://github.com/kovvbojAV/kovvboj)**. It stacks layers
of cameras, clips, shaders and text with per-layer and master FX chains, runs
two decks with ISF transitions, and drives projectors, LED strips and lasers.

It depends on the engine crates by git `rev`, so it is also the best reference
for how the pieces in this guide compose into a real app. Start with its
[architecture notes](https://github.com/kovvbojAV/kovvboj/blob/main/docs/architecture.md),
and read them alongside the [mixer](../rendering/render-graph.md) and
[lighting](../lighting.md) chapters.
