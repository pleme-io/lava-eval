//! lava-eval — in-memory tatara-lisp evaluator for lava architectures.
//!
//! Parses `.tlisp` source, evaluates `(deflava-architecture …)` forms,
//! produces typed [`lava_core::Architecture`]. Magma consumes this
//! directly to do plan/apply transformations in-memory — no on-disk
//! JSON between authoring and apply.
//!
//! Extracted from lava-architectures so magma-lava + future consumers
//! can load .tlisp without depending on the architecture corpus.
//!
//! ## Pipeline
//!
//! ```text
//! .tlisp source
//!     │ sexpr::parse → Sx
//!     │ eval::eval_architecture
//! lava_core::Architecture
//!     │ render_terraform_json
//! serde_json::Value (terraform.json shape)
//!     │ magma plan/apply
//! cloud state
//! ```

#![allow(clippy::module_name_repetitions)]

pub mod eval;
pub mod sexpr;

pub use eval::{eval_architecture, EvalError, InputBindings};
pub use sexpr::{parse, Atom, ParseError, Sx};
