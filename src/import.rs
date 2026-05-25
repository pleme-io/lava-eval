//! Typed reader for `(deflava-import …)` forms.
//!
//! An import names another architecture by name, optionally with an
//! alias + per-import binding overrides. Composition happens at the
//! consumer layer (lava-architectures + lava CLI know how to look up
//! architectures by name); this parser only extracts the typed Import
//! value.
//!
//! ## Form
//!
//! ```lisp
//! (deflava-import aws-vpc-network
//!   :as           network
//!   :bindings     (:name "preview"
//!                  :cidr "10.42.0.0/16"))
//!
//! (deflava-import cloudflare-dns-records
//!   :as     dns
//!   :bindings (:zone-id "11112222..."))
//! ```
//!
//! Consumers (lava-architectures' composer) walk imports_in_source,
//! resolve each `target` via bundled-architecture lookup, evaluate
//! the target with `bindings`, and merge the resulting resources +
//! outputs into the parent architecture under `alias` (if set).

use crate::sexpr::{parse_all, Sx};
use indexmap::IndexMap;
use thiserror::Error;

/// Typed import value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// Architecture name being imported.
    pub target: String,
    /// Optional alias. Resources/outputs under this prefix avoid
    /// collisions when multiple imports of the same target land in
    /// one parent.
    pub alias: Option<String>,
    /// Operator binding overrides layered on top of the imported
    /// architecture's defaults.
    pub bindings: IndexMap<String, String>,
}

impl Import {
    #[must_use]
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            alias: None,
            bindings: IndexMap::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ImportParseError {
    #[error("parse: {0}")]
    Parse(#[from] crate::sexpr::ParseError),
    #[error("expected deflava-import form")]
    NotImportForm,
    #[error("malformed deflava-import: {0}")]
    Malformed(String),
}

/// Scan a source string for every `(deflava-import …)` form and
/// return the typed [`Import`] for each one.
///
/// # Errors
/// Surfaces parse errors and per-import shape errors.
pub fn imports_in_source(src: &str) -> Result<Vec<Import>, ImportParseError> {
    let forms = parse_all(src)?;
    let mut out = Vec::new();
    for form in forms {
        let Some(xs) = form.as_list() else { continue };
        if xs.first().and_then(Sx::as_sym) == Some("deflava-import") {
            out.push(import_from_form(xs)?);
        }
    }
    Ok(out)
}

fn import_from_form(xs: &[Sx]) -> Result<Import, ImportParseError> {
    let target = xs
        .get(1)
        .and_then(Sx::as_sym)
        .or_else(|| xs.get(1).and_then(Sx::as_str))
        .ok_or(ImportParseError::NotImportForm)?
        .to_string();
    let mut import = Import::new(target);
    let mut i = 2;
    while i + 1 < xs.len() {
        match xs[i].as_kw() {
            Some("as") => {
                import.alias = xs[i + 1]
                    .as_sym()
                    .or_else(|| xs[i + 1].as_str())
                    .map(std::string::ToString::to_string);
            }
            Some("bindings") => {
                if let Some(pairs) = xs[i + 1].as_list() {
                    let mut j = 0;
                    while j + 1 < pairs.len() {
                        if let (Some(k), Some(v)) =
                            (pairs[j].as_kw(), pairs[j + 1].as_str())
                        {
                            import.bindings.insert(k.to_string(), v.to_string());
                        }
                        j += 2;
                    }
                }
            }
            _ => {}
        }
        i += 2;
    }
    Ok(import)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_in_source_extracts_typed_imports() {
        let src = r#"
            (deflava-import aws-vpc-network
              :as network
              :bindings (:name "preview" :cidr "10.42.0.0/16"))

            (deflava-import cloudflare-dns-records
              :as dns
              :bindings (:zone-id "abcdef1234567890"))
        "#;
        let imports = imports_in_source(src).unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].target, "aws-vpc-network");
        assert_eq!(imports[0].alias.as_deref(), Some("network"));
        assert_eq!(imports[0].bindings["cidr"], "10.42.0.0/16");
        assert_eq!(imports[1].target, "cloudflare-dns-records");
        assert_eq!(imports[1].bindings["zone-id"], "abcdef1234567890");
    }

    #[test]
    fn import_without_alias_or_bindings_parses_cleanly() {
        let src = "(deflava-import aws-vpc-network)";
        let imports = imports_in_source(src).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].target, "aws-vpc-network");
        assert!(imports[0].alias.is_none());
        assert!(imports[0].bindings.is_empty());
    }

    #[test]
    fn skips_non_import_forms_in_multi_form_source() {
        let src = r#"
            (deflava-interface demo :inputs ())
            (deflava-import aws-vpc-network :as net)
            (deflava-architecture demo :inputs () :resources ())
        "#;
        let imports = imports_in_source(src).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].target, "aws-vpc-network");
    }

    #[test]
    fn missing_target_surfaces_typed_error() {
        let src = "(deflava-import)";
        let err = imports_in_source(src).unwrap_err();
        matches!(err, ImportParseError::NotImportForm);
    }
}
