# Getting started writing applications for magpie

This folder contains programs and libraries running against magpie, and some guidelines for writing
your own.

## Sample Applications

There are currently two "applications" that have been implemented:
* `kvs` is a library that exposes a key/value store subsystem through three public nanotransactions:
  `kvs::put`, `kvs::get`, and `kvs::get_ref`. Other programs that use this library will use string
  keys (for now), but can provide their own `Persistable` type as the value type to be stored.
* `kvs-consumer` is a dummy program that uses the kvs to store some configuration information in the
  key/value store.

## Writing Your Own

At a minimum, you need two things to start writing a library:
- The `nandoize::nandoize_lib` macro that you attach to **functions** that you want to expose as
  nanotransactions.
- The `nandoize::PersistableDeriveLib` macro that you attach to **data types** that you want to
  store in objects.

**IMPORTANT** All struct types that are meant to be stored in objects need to be annotated with
`#[repr(C)]` for layout math to work correctly.

There is a `sample` library that you should probably use as your template when writing a new
library, as it includes the minimum useful set of imports to get started, as well as a build script
that is necessary when compiling nanotransaction libraries.

The main restriction for programs right now is that their crate type needs to be `dylib`.

### Workflows

The module `nando_support` exposes a set of macros that are used when constructing workflows (or
epics):
- `nando_spawn!` and `nando_spawn_polymorphic!` are used when you already have a list of arguments
  that you can supply to another nanotransaction from within the current nanotransaction (e.g.
  `kvs_consumer::get_latest_added_host()` invokes `kvs::get()` because it already knows the value of
  the `key` argument). You can use `nando_spawn!` to launch any number of nanotransactions that don't
  rely on each other's outputs in the body of another nanotransaction.
- `nando_yield!` and `nando_yield_polymorphic!` are used when some of the arguments that are being
  passed to the target nanotransaction are themselves results of other nanotransactions that have
  previously been spawned using one of the `nando_spawn` variants above. `nando_yield` statements
  should be the last statement executed for the given nanotransaction path.

Another way to thing about `nando_spawn!` and `nando_yield!` is as an analogy for futures-based
asynchronous programming -- spawn statements create futures, but those futures do not start
executing until they are "await"ed. `nando_yield!` is like an await on all issued futures up to that
point -- it represents the join point of all spawned tasks before the epic can proceed to the task
named by `nando_yield!`.

There can only ever be one `nando_yield` statement that is ever exercised as part of a
nanotransaction's body. Another way of saying this is that you can have one `nando_yield` per
disjoint path in a nanotransaction, but any given path can only contain one `nando_yield` as its
last statement (and any set of preceding `nando_spawn` statements, provided that they are not data
dependent).

### Generics and Nanotransactions

The `_polymorphic` variants of the above macros are used when the target of the spawn or yield is a
generic function, **or** when the target is not generic but it contains `_polymorphic` invocations
of nanotransactions. Their main difference to the undecorated variants is that they prepend the
namespace of the library that served as the entry point to the current nanotransaction to the
target. This means that the entry point for the invoked nanotransaction will be at a call site that
has knowledge of any type specializations that need to be applied before invoking the generic
target.

There are two ways for the magpie to figure out the types associated with the generic function and
to attach them to the generated call site:
- If the target nanotransaction's generic type variables can be inferred entirely by its output
  type, then simply assigning a type to the target of the polymorphic spawn will suffice.
- If the above is not the case, then you can add the type annotation as part of the target; see
  `kvs_consumer::put_i32()` for an example.

Regardless of the case, the output of the above statements will be typechecked down the line, and
you will get a (potentially obscure, this is still early days) error from the rust compiler
signaling a type-level issue.

### Allocating Objects

The `kvs` library (and more specifically `kvs::init_buckets()`) is the best place to start to see
how you can declare data structures that are stored in objects, as well as how object data is
actually allocated.

## Running and invoking through magpie

Once your library builds cleanly, you can load it into magpie by adding a new mapping to the
`nando_lib_config.scheduler_config.library_paths` key of your [configuration
file](../magpie/config.toml). The key used should be the identifier of the namespace under
which the present library's nanotransactions will be available.

You can test your authored nanotransactions by submitting requests to `/activation_router/schedule`
to a local magpie instance. For example, the following command  will execute the `put_i32()`
nanotransaction declared under the `kvs_consumer` namespace.

```bash
curl -X POST 127.0.0.1:52017/activation_router/schedule -H 'Content-Type: application/json' \
 -d '{"name": "kvs_consumer::put_i32", "args": [{"Value": "key-1-913"}, {"Value": 42}]}'
 ```

## Important Note For Correct Linking

### Linux Users

On Linux, you will probably have to change the `LD_LIBRARY_PATH` variable in the shell running
magpie to esure all necessary `*.so` libraries are visible when executing. My current way of doing
this is to prepend the `target` subdirectory of the top-level libraries I want to invoke through
magpie. For example, to run `kvs-consumer`, I would run:

```bash
$ export LD_LIBRARY_PATH=<absolute path to kvs-consumer/target/profile>:$LD_LIBRARY_PATH
```

### macOS Users

If you're running on macOS, you will probably have to set `DYLD_LIBRARY_PATH` in order for library
loading to work correctly. With the current toolchain, the emitted libraries link to std using an
`@rpath` path, but unfortunately the generated `.dylib` files contain no `LC_RPATH` commands, which
means loading these libraries will fail on magpie startup with an error like:

```
DlOpen : ..., Library not loaded: @rpath/libstd-57b8b00dd0c2eb88.dylib
```

. To fix this issue, simply run
```bash
$ export DYLD_LIBRARY_PATH="$(rustc --print sysroot)/lib:$DYLD_LIBRARY_PATH"
```

before attempting to run magpie.
