# figaro: ownership orchestrator

## Structure

The two main parts of the repository are the following:
- `handers.rs`, which contains the scheduler entry points -- these are the endpoints used by workers.
- `orchestration`, which contains all of the ownership state maintenance code, together with the cost model
  logic and endpoints used during ownership transfer.

## Development

### Prerequisites

Before your first commit, make sure to run
```bash
$ pre-commit install
```

in the repo's root folder.

### How to build

To build the project using the affinity-based cost model (that minimizes the total number of object
moves for a given computation), simply run:
```bash
$ cargo build [--release]
```

To instead build using the eager always-move strategy, run:
```bash
$ cargo build [--release] --features always_move
```
