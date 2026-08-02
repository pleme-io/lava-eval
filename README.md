# lava-eval

In-memory tatara-lisp evaluator for lava architectures.

Parses `.tlisp` source, evaluates `(deflava-architecture ...)` forms, and
produces a typed `lava_core::Architecture`.
[magma](https://github.com/pleme-io/magma) consumes this directly to run
plan/apply **in memory** — there is no on-disk JSON between authoring and
apply.

Extracted from
[`lava-architectures`](https://github.com/pleme-io/lava-architectures) so that
`magma-lava` and future consumers can reuse it.

## Install

```toml
[dependencies]
lava-eval = "0.1"
```

## The suite

```
lava-core ──┐
lava-arch ──┼──► lava-eval ──► lava-runtime
lava-schema ┤
lava-types ─┘
```

## License

MIT
