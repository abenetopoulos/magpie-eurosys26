# Magpie: execution worker

## Requirements

- `rustc` and `cargo` (>= 1.64.0)
    - Note that most components in this repo override the system rust toolchain because we rely on
      nightly features, so meeting the above dependency is not super critical; the main thing you
      need is a working installation of the rust toolchain.
- `pre-commit` (>= 2.20.0), installation instructions [here](https://pre-commit.com/)
- `protoc` (>= 3.21.9), installation instructions [here](https://grpc.io/docs/protoc-installation/)

### Structure

The runtime is composed of the `magpie` "client" (build instructions [here](#how-to-build)) and the following libraries:
- `object-lib`: The data layer of the runtime. Exposes the object interface to nandos and the client.
- `nando-lib`: User-level nanotransaction declaration/registration facilities, as well as the client's interface to the
  underlying execution engine.
  definitions/registrations.
- `ownership-tracker`: The object ownership subsystem, along with its interface to the client.
- `location-manager`: The object placement subsystem, along with its interface to the client.

## Configuration

You can pass a TOML configuration file to magpie using the `-c` argument. An example file is
provided (under `magpie/config.toml`), which you can copy and adapt to your purposes.

If you have compiled magpie with the `offline` feature enabled, then it doesn't matter if the
ownership orchestrator is running.

### Configuring the number of subsystem threads

Magpie operates under a thread-per-core model. Five subsystems spawn threads to perform work, which
are pinned to a given core:

* Each worker's local scheduler, which has two components, one submission thread (responsible for picking
  an appropriate executor thread for each nanotransaction) and one or more completion threads
  (responsible for processing completed nanotransactions and taking further action).
* Each worker's executor thread pool.
* Each worker's logging subsystem.
* The "frontend" of magpie workers, which is managed by tokio, responsible for bridging the synchronous
  execution system with the outside world.
* Optionally, the telemetry subsystem.

There are always exactly **one** submission, logging, and telemetry threads, but all other
subsystems can be configured with a variable number of threads. Note that, for best performance, the
total number of threads across all subsystems (including the telemetry thread even if compiling
without the `telemetry` feature) should be equal to the number of **cores** (not
threads) available on the machine the worker is running on, and also SMT needs to be disabled. The
executor thread pool (specified under `[nando_lib_config.executor_config]`) should be given the
highest number of threads, followed by the scheduler completion thread pool (specified under
`[nando_lib_config.scheduler_config]`). The remaining amount of cores should be assigned to the
tokio thread pool (specified under `[async_lib_config]`).

## Development

### Prerequisites

Before your first commit, make sure to run
```bash
$ pre-commit install
```

in the repo's root folder.

### How to build

You can build any of the above libraries independently by `cd`ing into its root directory and running
```bash
$ cargo build [--release]
```

To build all subcomponents along with the client, execute the above command from the `magpie`
directory, along with any feature flags.

If you are just building for local development, run the following command instead:

```bash
$ cargo build [--release] --features offline
```

#### Feature Flags

Magpie supports a range of feature flags, with the most important explained below.

* `offline`: Building magpie with the `offline` flag produces a binary that is meant to execute on a
  single machine for testing purposes; you do not need a running `figaro` instance or any other
  worker. Note that if you build the runtime with the `offline` flag you should also build the
  application you are interested in running using the `offline` flag (see `applications/kvs` for an
  example of a `Cargo.toml` file to support this).
* `timing-*`: This is a set of feature flags that enables timing information for the various magpie
  subsystems (look into `magpie/Cargo.toml` for a complete list of flags). Note that enabling a
  variety of these can severely affect performance; as a result these are supposed to be used only
  for performance debugging purposes.

### Documentation

To generate the documentation page for the runtime and scheduler, run:
```bash
$ ./cargo-docs
```

from the project's root directory. Passing the `-h` flag to the script displays its usage string.
