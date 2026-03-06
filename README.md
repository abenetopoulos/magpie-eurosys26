# Magpie

This repo contains the source code for Magpie (paper title "Yield Not Thy Core") presented at
[EuroSys '26](https://2026.eurosys.org/index.html).

The source code of magpie (the runtime as a whole) is split up between two workspaces:
- `magpie`, which contains the parts of the runtime that execute on a worker (nanotransaction
  "compilation", execution, etc.)
- `figaro`, which contains the code for the "global ownership orchestrator".

Under `applications` you will find the nanotransaction libraries that have already been implemented.

## Prerequisites

In order to be able to generate documentation, you will first have to execute

```bash
git submodule --init --update-recursive
```

from the repo's root. You will then be able to execute `./cargo-docs -o` from the root, which will
open your browser to the landing page for the crate-level documentation of the runtime.
