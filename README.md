# TSGL_rust

Rust ports of the TSGL concurrency visualizations:

- `ReaderWriter`
- `ProducerConsumer`
- `DiningPhilosophers`

The original TSGL project is a C++/OpenGL teaching library for parallel and synchronization demos. This repository reimplements the visualizations in Rust using:

- `eframe` / `egui` for the UI
- `std::thread` for worker threads
- `parking_lot` synchronization primitives

The Rust ports are structured so that simulation logic and UI rendering are separated:

- worker threads update shared simulation state
- the UI reads snapshots of that state and renders them
- worker threads do not draw directly

## Requirements

- Rust toolchain
- Cargo

## Build

```bash
cargo check
```

## Run

### ReaderWriter

```bash
cargo run --bin reader_writer -- [numReaders] [numWriters] [policy] [starved]
```

Arguments:

- `numReaders`: number of reader threads, default `6`
- `numWriters`: number of writer threads, default `6`
- `policy`:
  - `r` = reader priority
  - `w` = writer priority
  - omitted or anything else = fair
- `starved`:
  - `s` = starvation timing mode
  - omitted or anything else = normal timing

Examples:

```bash
cargo run --bin reader_writer -- 2 1
cargo run --bin reader_writer -- 6 6 r
cargo run --bin reader_writer -- 6 6 w s
```

Controls:

- `Space` = pause / resume
- `Reset` button = restart the simulation

### ProducerConsumer

```bash
cargo run --bin producer_consumer -- [numProducers] [numConsumers]
```

Arguments:

- `numProducers`: number of producers, default `5`
- `numConsumers`: number of consumers, default `5`

Behavior matches the original TSGL demo:

- if either value is `<= 0`, both reset to `8`
- if either value is `> 8`, both reset to `8`

Examples:

```bash
cargo run --bin producer_consumer -- 2 1
cargo run --bin producer_consumer -- 5 5
```

Controls:

- `Space` = pause / resume
- `Reset` button = restart the simulation

### DiningPhilosophers

```bash
cargo run --bin dining_philosophers -- [numPhilosophers] [speed|t|y] [method]
```

Arguments:

- `numPhilosophers`: number of philosophers, default `5`
- `speed|t|y`:
  - positive integer = animation speed
  - `t` or `y` = step-through mode
- `method`:
  - `w` = wait when blocked
  - `f` = forfeit when blocked
  - `n` = n-count release
  - `r` = resource hierarchy
  - `o` = odd-even
  - default = `o`

Examples:

```bash
cargo run --bin dining_philosophers -- 5 5 o
cargo run --bin dining_philosophers -- 7 10 r
cargo run --bin dining_philosophers -- 5 t f
```

Controls:

- normal mode: `Space` = pause / resume
- step-through mode: `Space` = advance one step
- `Reset` button = restart the simulation

## Project Layout

```text
src/
  reader_writer/
    main.rs
  producer_consumer/
    main.rs
  dining_philosophers/
    main.rs
```

Each binary is self-contained and can be run independently.

## Known Differences From TSGL

These Rust ports preserve the teaching goals and core synchronization behavior of the original TSGL demos, but they are not line-for-line translations.

Notable differences:

- Rendering is done with `egui` instead of TSGL/OpenGL drawables.
- Worker threads do not draw directly; they only update shared simulation state.
- Some visuals were adapted to fit immediate-mode UI rendering.
- The Rust versions favor explicit shared state and cooperative shutdown instead of the original pthread/OpenMP rendering model.
- In a few places, behavior was adjusted for clearer UI semantics while keeping the original demo intent.

## License

The original TSGL repository is licensed under GPL v3. Because this repository is a Rust port of those visualizations, `TSGL_rust` should be treated as GPL-compatible work as well unless you have a clean-room legal basis to license it differently.

If you are publishing this repository, add a GPL v3 license file.
