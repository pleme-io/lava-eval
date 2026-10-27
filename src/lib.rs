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

pub mod dashboard;
pub mod eval;
pub mod import;
pub mod interface;
pub mod leaf;
pub mod sexpr;
pub mod variables;

pub use dashboard::{eval_dashboard, render_dashboard_grafana_json};
pub use eval::{
    EvalError, InputBindings, Scalar, eval_architecture, eval_architecture_with_schema,
};
pub use import::{Import, ImportParseError, imports_in_source};
pub use interface::{InterfaceParseError, interface_from_form, interfaces_in_source};
pub use variables::VariableError;
pub use leaf::{LeafKey, LeafParseError, SliceError, leaf_key_in_source};
pub use sexpr::{Atom, ParseError, Sx, parse, parse_all};
