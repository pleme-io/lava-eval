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
pub mod import;
pub mod interface;
pub mod sexpr;

pub use eval::{eval_architecture, eval_architecture_with_schema, EvalError, InputBindings};
pub use import::{imports_in_source, Import, ImportParseError};
pub use interface::{interface_from_form, interfaces_in_source, InterfaceParseError};
pub use sexpr::{parse, parse_all, Atom, ParseError, Sx};
