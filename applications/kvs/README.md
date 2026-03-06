## Authoring nanotransactions

The nanotransactions that comprise the KVS implementation can be found in `src/lib.rs`, annotated
with `#[nandoize_lib]`.

To start writing your own nanotransaction library, copy the local dependencies from `Cargo.toml`
into your own project.

In your own library, you will need the following imports at the very least:
- `nandoize::{nandoize_lib, PersistableDeriveLib}`: macros used to process functions into
  nanotransaction definitions and to provide data definitions (e.g. structs) with default
  implementations for persistable object types.
- If you are planning on using any of the provided collection types (`object_lib::collections::*`),
  you will also need to import the `PersistentlyAllocatable` trait from `object_lib` in order to
  implement allocator instantiation for instances of your object types.

**IMPORTANT** All defined struct types need to be annotated with `#[repr(C)]` for layout math to
work correctly.

In your `lib.rs` file you will also need to include the line:
```rust
pub mod resolver;
```
