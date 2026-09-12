# pdf_annotation_extractor playground

[Try it](https://saona-raimundo.github.io/pdf_annotation_extractor/) — drop an
annotated PDF and read the report. The document is never uploaded.

## Development

```sh
just setup    # once: wasm32 target and trunk
just serve    # http://localhost:8080, reloads on change
just check    # fmt and clippy, on the wasm target
just build    # release bundle into ../docs, as CI publishes it
```

- Framework: [leptos](https://leptos.dev/) (client-side rendering)
- Bundler: [trunk](https://trunkrs.dev/)
- Runner: [just](https://github.com/casey/just)

### Folders

- `src/job.rs` — everything that touches the library: `Options`, `Style`,
  serialisation, error formatting. Read this to see what a consumer has to
  write.
- `src/main.rs` — the interface, which only moves strings around.
- `assets/` — the stylesheet.
- `../docs` — build output, git-ignored. Published by CI, never committed;
  a 1 MB WebAssembly blob that changes on every release does not belong in
  the history of a five-dependency crate.

### It is its own workspace

`playground/Cargo.toml` carries an empty `[workspace]` table. Folded into the
parent instead, the root `Cargo.lock` would gain leptos and wasm-bindgen,
`cargo clippy --all-targets` at the root would lint the interface, and the MSRV
job would start measuring the playground's floor rather than the library's.
