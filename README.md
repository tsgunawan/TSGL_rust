# TSGL Rust

Rust ports of the TSGL concurrency visualization demos, redesigned with a clean
light theme suitable for academic publications (IEEE / ACM paper figures).

Repository: <https://github.com/tsgunawan/TSGL_rust>

The original TSGL project is a C++ / OpenGL teaching library for parallel
programming and synchronization demos. This repository reimplements the three
core visualizations in Rust using **eframe / egui** for rendering and
**std::thread** + **parking\_lot** for concurrency.

## Visualizations

| Binary | Demonstrates |
|---|---|
| `reader_writer` | Reader–writer lock with reader-priority, writer-priority, and fair policies |
| `producer_consumer` | Bounded-buffer producer–consumer with a circular 8-slot queue |
| `dining_philosophers` | Dining philosophers with five deadlock-avoidance strategies |

## Author

**Prof. Dr. Teddy Surya Gunawan**  
Electrical and Computer Engineering Department  
International Islamic University Malaysia  
tsgunawan@iium.edu.my

## Course Context

Prepared for Operating Systems class use.

## Acknowledgements

Rust port of the TSGL concurrency visualizations.  
Special thanks to the original TSGL authors for their teaching visualizations.

## Citation

If you use this repository, please cite:

Teddy Surya Gunawan, "TSGL-Rust: Teaching Concurrency Through Compile-Time Enforcement via Rust-Based Visualization of Classical Synchronization Problems", *IIUM Engineering Journal*, Vol. 27, Issue 2, 2026. DOI: <https://doi.org/10.31436/iiumej.v27i2.4361>

---

## Requirements

- Rust toolchain (edition 2024, stable)
- Cargo

## Build

```bash
cargo build --release
```

## Run

### Reader–Writer

```bash
cargo run --bin reader_writer -- [numReaders] [numWriters] [policy] [starved]
```

| Argument | Values | Default |
|---|---|---|
| `numReaders` | 1–9 | 6 |
| `numWriters` | 1–9 | 6 |
| `policy` | `r` reader-priority · `w` writer-priority · anything else fair | fair |
| `starved` | `s` starvation timing · anything else normal | normal |

```bash
cargo run --bin reader_writer -- 2 1
cargo run --bin reader_writer -- 6 6 r
cargo run --bin reader_writer -- 6 6 w s
```

### Producer–Consumer

```bash
cargo run --bin producer_consumer -- [numProducers] [numConsumers]
```

| Argument | Values | Default |
|---|---|---|
| `numProducers` | 1–8 | 5 |
| `numConsumers` | 1–8 | 5 |

If either value is ≤ 0 or > 8, both are clamped to 8.

```bash
cargo run --bin producer_consumer -- 2 1
cargo run --bin producer_consumer -- 5 5
```

### Dining Philosophers

```bash
cargo run --bin dining_philosophers -- [numPhilosophers] [speed|t|y] [method]
```

| Argument | Values | Default |
|---|---|---|
| `numPhilosophers` | ≥ 2 | 5 |
| `speed\|t\|y` | positive integer = speed · `t` or `y` = step-through | 5 |
| `method` | `w` wait · `f` forfeit · `n` n-count · `r` hierarchy · `o` odd-even | `o` |

```bash
cargo run --bin dining_philosophers -- 5 5 o
cargo run --bin dining_philosophers -- 7 10 r
cargo run --bin dining_philosophers -- 5 t f
```

---

## Controls

| Key / Button | Action |
|---|---|
| `Space` | Pause / resume (step-through mode: advance one step) |
| `S` | **Freeze threads and save a PNG screenshot** |
| `Reset` button | Stop all threads and restart the simulation |

### Screenshot feature

Pressing `S` pauses the simulation and writes the current window contents to a
PNG file in the working directory:

```
reader_writer01.png
reader_writer02.png   ← counter increments on each press
producer_consumer01.png
dining_philosophers01.png
```

A confirmation is printed to the terminal:

```
Screenshot saved: reader_writer01.png
```

Screenshots capture the window at its current state — threads may be mid-
execution in a mix of reading, writing, waiting, and thinking states, which
produces the most informative figure for a paper.  Press `Space` to resume
after capturing.

---

## Visualization Design

All three programs share a light academic color theme optimized for inclusion in
IEEE / ACM conference and journal figures.

### Color palette

| State | Color | Hex |
|---|---|---|
| Active / running | Blue | `#2563eb` |
| Waiting / blocked | Orange | `#ea580c` |
| Eating / success | Green | `#16a34a` |
| Idle / thinking | Gray | `#6b7280` |
| Writing (reader–writer) | Red | `#dc2626` |
| Background | White | `#ffffff` |
| Panel background | Light gray | `#f8f9fa` |

### Design choices

- **Flat colors only** — no gradients, no transparency; renders cleanly in PDF
  screenshots and grayscale print.
- **Minimum font size 12 pt** — all labels remain legible when a figure is
  scaled down to a single column width.
- **30 fps repaint** — smooth enough for live demonstration, slow enough for
  clean screenshots without motion artifacts.
- **No pulse animations** — state transitions use solid color changes, not
  sin-wave flicker, so any frame is a publication-ready still.

### Per-visualization notes

**Reader–Writer** — writers are highlighted with a flat red border around the
data store while holding the lock; readers show a green active-reader badge.
Thread circles move between home → waiting → access positions.

**Producer–Consumer** — the 8-slot circular buffer is divided by visible radial
lines with slot numbers 1–8; a fill-bar below shows buffer occupancy; animated
stars travel from producers to the queue and from the queue to consumers.

**Dining Philosophers** — philosophers sit around a light warm table; fork
positions reflect ownership (pulled toward the holder); meal dots accumulate
around each philosopher to show progress over time.

---

## Architecture

Each binary is fully self-contained in `src/<name>/main.rs` with no shared
library code between them.

```
src/
  reader_writer/main.rs
  producer_consumer/main.rs
  dining_philosophers/main.rs
```

**Threading model (consistent across all three):**

- Worker threads run simulation logic and update shared state via
  `Arc<Mutex<…>>` or `Arc<RwLock<…>>` from `parking_lot`.
- The `eframe` UI loop reads a snapshot of that state each frame and renders
  it — workers never call drawing functions directly.
- A shared `AtomicBool` coordinates pause / resume between threads and the UI.
- On reset, all threads are signalled to stop, joined, then re-spawned with
  fresh state.

**Synchronization primitives:**

| Primitive | Used for |
|---|---|
| `parking_lot::Mutex` + `Condvar` | Blocking workers on full/empty buffer or lock contention |
| `parking_lot::RwLock` | Shared simulation snapshots read by the UI |
| `std::sync::atomic::AtomicBool` | `running` / `paused` flags |
| `std::sync::Barrier` | Philosopher thread startup sync (forfeit method) |

---

## Known Differences From TSGL

These ports preserve the teaching goals and synchronization behavior of the
original TSGL demos but are not line-for-line translations.

- Rendering uses **egui** instead of TSGL / OpenGL drawables.
- Worker threads update shared state; the UI reads and renders snapshots.
- Some visuals were adapted for immediate-mode UI rendering.
- Cooperative shutdown replaces the original pthread / OpenMP rendering model.

---

## License

The original TSGL repository is licensed under **GPL v3**. This repository, as
a port of those visualizations, should be treated as GPL v3-compatible work.
