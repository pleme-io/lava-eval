//! Typed reader for `(deflava-interface …)` forms.
//!
//! Parses the same s-expr shape the .tlisp architectures author and
//! produces typed [`lava_schema::Interface`] values. Replaces the
//! hand-curated Rust mirror in `lava-architectures` — the .tlisp
//! source becomes the single source of truth.
//!
//! ## Form shape
//!
//! ```lisp
//! (deflava-interface aws-vpc-network
//!   :doc "Standard AWS VPC + IGW + subnets + NAT"
//!   :inputs ((:cidr   :type :cidr-block :default "10.0.0.0/16")
//!            (:azs    :type (:list-of :availability-zone)
//!                     :min-items 1 :default ("us-east-1a"))
//!            (:env    :type (:enum "prod" "staging" "dev") :default "prod"))
//!   :outputs ((:vpc-id            :type :string :required #t)
//!             (:public-subnet-ids :type (:list-of :string))))
//! ```
//!
//! ## Type keyword mapping
//!
//! Atom keywords → primitives: `:string`, `:integer`, `:boolean`,
//! `:any`, `:dynamic`, `:cidr-block`, `:ipv4`, `:ipv6`, `:hostname`,
//! `:port-range`, `:availability-zone`, `:email`, `:url`.
//!
//! Form keywords → parametric: `(:list-of <type> :min-items N
//! :max-items M)`, `(:enum "a" "b" ...)`, `(:int-range :min N :max M)`,
//! `(:length :min N :max M)`, `(:pattern :match-kind contains
//! :value "...")`.

use crate::sexpr::{parse_all, Atom, Sx};
use lava_schema::{Field, Interface};
use lava_types::{MatchKind, Type};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InterfaceParseError {
    #[error("parse: {0}")]
    Parse(#[from] crate::sexpr::ParseError),
    #[error("expected deflava-interface form, got {0}")]
    NotInterfaceForm(String),
    #[error("missing :{0} clause")]
    MissingClause(&'static str),
    #[error("malformed field declaration: {0}")]
    BadField(String),
    #[error("unknown type keyword `{0}`")]
    UnknownType(String),
    #[error("malformed type form: {0}")]
    BadTypeForm(String),
}

/// Scan a source string for every `(deflava-interface …)` form and
/// return the typed [`Interface`] for each one. Ignores
/// `(deflava-architecture …)` siblings.
///
/// # Errors
/// Surfaces parse errors and per-interface conversion errors.
pub fn interfaces_in_source(src: &str) -> Result<Vec<Interface>, InterfaceParseError> {
    let forms = parse_all(src)?;
    let mut out = Vec::new();
    for form in forms {
        let Some(xs) = form.as_list() else { continue };
        if xs.first().and_then(Sx::as_sym) == Some("deflava-interface") {
            out.push(interface_from_form(xs)?);
        }
    }
    Ok(out)
}

/// Convert one `(deflava-interface …)` form into a typed Interface.
///
/// # Errors
/// Returns [`InterfaceParseError`] for missing fields / bad types.
pub fn interface_from_form(xs: &[Sx]) -> Result<Interface, InterfaceParseError> {
    let name = xs
        .get(1)
        .and_then(Sx::as_sym)
        .ok_or_else(|| InterfaceParseError::NotInterfaceForm("missing name".into()))?
        .to_string();
    let mut iface = Interface::new(name);

    let mut i = 2;
    while i + 1 < xs.len() {
        let key = xs[i].as_kw();
        let val = &xs[i + 1];
        match key {
            Some("doc") => {
                iface.doc = val.as_str().map(std::string::ToString::to_string);
            }
            Some("inputs") => {
                let fields = val
                    .as_list()
                    .ok_or_else(|| InterfaceParseError::BadField(":inputs not a list".into()))?;
                for f in fields {
                    let (name, field) = field_from_form(f)?;
                    iface.inputs.insert(name, field);
                }
            }
            Some("outputs") => {
                let fields = val
                    .as_list()
                    .ok_or_else(|| InterfaceParseError::BadField(":outputs not a list".into()))?;
                for f in fields {
                    let (name, field) = field_from_form(f)?;
                    iface.outputs.insert(name, field);
                }
            }
            // Unknown clauses pass silently — gives the format room
            // to grow without breaking existing readers.
            _ => {}
        }
        i += 2;
    }

    Ok(iface)
}

fn field_from_form(form: &Sx) -> Result<(String, Field), InterfaceParseError> {
    let xs = form.as_list().ok_or_else(|| {
        InterfaceParseError::BadField(format!("field not a list: {form:?}"))
    })?;
    let name = xs
        .first()
        .and_then(Sx::as_kw)
        .ok_or_else(|| InterfaceParseError::BadField("field head must be :kw".into()))?
        .to_string();

    // Walk keyword pairs from position 1.
    let mut ty: Option<Type> = None;
    let mut required = false;
    let mut default: Option<String> = None;
    let mut doc: Option<String> = None;
    let mut min_items: Option<usize> = None;
    let mut max_items: Option<usize> = None;

    let mut i = 1;
    while i + 1 < xs.len() {
        let key = xs[i].as_kw();
        let val = &xs[i + 1];
        match key {
            Some("type") => {
                ty = Some(type_from_form(val)?);
            }
            Some("required") => {
                required = val.as_bool().unwrap_or(false);
            }
            Some("default") => {
                default = render_default(val);
            }
            Some("doc") => {
                doc = val.as_str().map(std::string::ToString::to_string);
            }
            Some("min-items") => {
                min_items = val.as_int().and_then(|n| usize::try_from(n).ok());
            }
            Some("max-items") => {
                max_items = val.as_int().and_then(|n| usize::try_from(n).ok());
            }
            _ => {}
        }
        i += 2;
    }

    let mut ty = ty.unwrap_or(Type::Any);
    // Lift inline :min-items / :max-items into the list-of constraint
    // when the field's type is a list-of.
    if let Type::ListOf { inner, min_items: mi, max_items: ma } = &mut ty {
        if min_items.is_some() {
            *mi = min_items;
        }
        if max_items.is_some() {
            *ma = max_items;
        }
        let _ = inner; // appease clippy
    }

    let loose = matches!(ty, Type::Any | Type::Dynamic);
    Ok((
        name,
        Field {
            r#type: ty,
            required,
            default,
            doc,
            loose,
        },
    ))
}

/// Render a default expression to its string representation. Strings,
/// integers, booleans, and flat string-lists all collapse to the
/// IndexMap<String,String> shape lava-schema accepts.
fn render_default(v: &Sx) -> Option<String> {
    match v {
        Sx::Atom(Atom::Str(s)) | Sx::Atom(Atom::Sym(s)) => Some(s.clone()),
        Sx::Atom(Atom::Bool(b)) => Some(b.to_string()),
        Sx::Atom(Atom::Int(n)) => Some(n.to_string()),
        Sx::List(items) => {
            // Flat list-of-strings → comma-joined (matches the
            // projection lava-runtime uses for list bindings).
            let parts: Vec<String> = items
                .iter()
                .filter_map(|x| match x {
                    Sx::Atom(Atom::Str(s)) | Sx::Atom(Atom::Sym(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(","))
            }
        }
        _ => None,
    }
}

fn type_from_form(v: &Sx) -> Result<Type, InterfaceParseError> {
    match v {
        Sx::Atom(Atom::Kw(k)) => primitive_type(k),
        Sx::List(items) => parametric_type(items),
        other => Err(InterfaceParseError::BadTypeForm(format!("{other:?}"))),
    }
}

fn primitive_type(k: &str) -> Result<Type, InterfaceParseError> {
    Ok(match k {
        "string" => Type::String,
        "integer" => Type::Integer,
        "boolean" => Type::Boolean,
        "any" => Type::Any,
        "dynamic" => Type::Dynamic,
        "cidr-block" => Type::CidrBlock,
        "ipv4" => Type::Ipv4,
        "ipv6" => Type::Ipv6,
        "hostname" => Type::Hostname,
        "availability-zone" => Type::AvailabilityZone,
        "email" => Type::Email,
        "url" => Type::Url,
        other => return Err(InterfaceParseError::UnknownType(other.to_string())),
    })
}

fn parametric_type(items: &[Sx]) -> Result<Type, InterfaceParseError> {
    let head = items
        .first()
        .and_then(Sx::as_kw)
        .ok_or_else(|| InterfaceParseError::BadTypeForm("parametric head must be :kw".into()))?;
    match head {
        "list-of" => {
            let inner_sx = items
                .get(1)
                .ok_or_else(|| InterfaceParseError::BadTypeForm(":list-of missing inner".into()))?;
            let inner = type_from_form(inner_sx)?;
            let mut min_items: Option<usize> = None;
            let mut max_items: Option<usize> = None;
            let mut i = 2;
            while i + 1 < items.len() {
                match items[i].as_kw() {
                    Some("min-items") => {
                        min_items = items[i + 1].as_int().and_then(|n| usize::try_from(n).ok());
                    }
                    Some("max-items") => {
                        max_items = items[i + 1].as_int().and_then(|n| usize::try_from(n).ok());
                    }
                    _ => {}
                }
                i += 2;
            }
            Ok(Type::ListOf {
                inner: Box::new(inner),
                min_items,
                max_items,
            })
        }
        "enum" => {
            let values: Vec<String> = items[1..]
                .iter()
                .filter_map(|x| match x {
                    Sx::Atom(Atom::Str(s)) | Sx::Atom(Atom::Sym(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            Ok(Type::Enum { values })
        }
        "int-range" => {
            let mut lo: Option<i64> = None;
            let mut hi: Option<i64> = None;
            let mut i = 1;
            while i + 1 < items.len() {
                match items[i].as_kw() {
                    Some("lo" | "min") => lo = items[i + 1].as_int(),
                    Some("hi" | "max") => hi = items[i + 1].as_int(),
                    _ => {}
                }
                i += 2;
            }
            Ok(Type::IntRange {
                lo: lo.ok_or_else(|| {
                    InterfaceParseError::BadTypeForm(":int-range needs :lo".into())
                })?,
                hi: hi.ok_or_else(|| {
                    InterfaceParseError::BadTypeForm(":int-range needs :hi".into())
                })?,
            })
        }
        "port-range" => {
            let mut lo: Option<u16> = None;
            let mut hi: Option<u16> = None;
            let mut i = 1;
            while i + 1 < items.len() {
                match items[i].as_kw() {
                    Some("lo" | "min") => lo = items[i + 1].as_int().and_then(|n| u16::try_from(n).ok()),
                    Some("hi" | "max") => hi = items[i + 1].as_int().and_then(|n| u16::try_from(n).ok()),
                    _ => {}
                }
                i += 2;
            }
            Ok(Type::PortRange {
                lo: lo.ok_or_else(|| {
                    InterfaceParseError::BadTypeForm(":port-range needs :lo".into())
                })?,
                hi: hi.ok_or_else(|| {
                    InterfaceParseError::BadTypeForm(":port-range needs :hi".into())
                })?,
            })
        }
        "length" => {
            let mut min: Option<usize> = None;
            let mut max: Option<usize> = None;
            let mut i = 1;
            while i + 1 < items.len() {
                match items[i].as_kw() {
                    Some("min") => {
                        min = items[i + 1].as_int().and_then(|n| usize::try_from(n).ok());
                    }
                    Some("max") => {
                        max = items[i + 1].as_int().and_then(|n| usize::try_from(n).ok());
                    }
                    _ => {}
                }
                i += 2;
            }
            Ok(Type::Length {
                min: min.ok_or_else(|| {
                    InterfaceParseError::BadTypeForm(":length needs :min".into())
                })?,
                max: max.ok_or_else(|| {
                    InterfaceParseError::BadTypeForm(":length needs :max".into())
                })?,
            })
        }
        "pattern" => {
            let mut source = String::new();
            let mut match_kind = MatchKind::Contains;
            let mut i = 1;
            while i + 1 < items.len() {
                match items[i].as_kw() {
                    Some("source" | "value") => {
                        source = items[i + 1]
                            .as_str()
                            .map(std::string::ToString::to_string)
                            .unwrap_or_default();
                    }
                    Some("match-kind") => {
                        match_kind = items[i + 1]
                            .as_sym()
                            .map(|s| match s {
                                "starts-with" => MatchKind::StartsWith,
                                "ends-with" => MatchKind::EndsWith,
                                _ => MatchKind::Contains,
                            })
                            .unwrap_or(MatchKind::Contains);
                    }
                    _ => {}
                }
                i += 2;
            }
            Ok(Type::Pattern { source, match_kind })
        }
        other => Err(InterfaceParseError::UnknownType(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_interface_with_string_and_cidr_inputs() {
        let src = r#"
            (deflava-interface aws-vpc-network
              :doc "Test VPC interface"
              :inputs ((:cidr :type :cidr-block :default "10.0.0.0/16")
                       (:env  :type :string :required #t))
              :outputs ((:vpc-id :type :string :required #t)))
        "#;
        let ifaces = interfaces_in_source(src).unwrap();
        assert_eq!(ifaces.len(), 1);
        let iface = &ifaces[0];
        assert_eq!(iface.name, "aws-vpc-network");
        assert_eq!(iface.doc.as_deref(), Some("Test VPC interface"));
        assert_eq!(iface.inputs.len(), 2);
        assert_eq!(iface.outputs.len(), 1);
        assert!(matches!(iface.inputs["cidr"].r#type, Type::CidrBlock));
        assert_eq!(iface.inputs["cidr"].default.as_deref(), Some("10.0.0.0/16"));
        assert!(iface.inputs["env"].required);
    }

    #[test]
    fn parses_list_of_with_min_items_and_typed_inner() {
        let src = r#"
            (deflava-interface stuff
              :inputs ((:azs :type (:list-of :availability-zone :min-items 1 :max-items 6))))
        "#;
        let ifaces = interfaces_in_source(src).unwrap();
        let f = &ifaces[0].inputs["azs"];
        match &f.r#type {
            Type::ListOf {
                inner,
                min_items,
                max_items,
            } => {
                assert!(matches!(**inner, Type::AvailabilityZone));
                assert_eq!(*min_items, Some(1));
                assert_eq!(*max_items, Some(6));
            }
            other => panic!("expected ListOf, got {other:?}"),
        }
    }

    #[test]
    fn parses_enum_and_collects_string_values() {
        let src = r#"
            (deflava-interface x
              :inputs ((:env :type (:enum "prod" "staging" "dev") :default "prod")))
        "#;
        let ifaces = interfaces_in_source(src).unwrap();
        let f = &ifaces[0].inputs["env"];
        match &f.r#type {
            Type::Enum { values } => {
                assert_eq!(values, &vec!["prod".to_string(), "staging".into(), "dev".into()]);
            }
            other => panic!("expected Enum, got {other:?}"),
        }
        assert_eq!(f.default.as_deref(), Some("prod"));
    }

    #[test]
    fn unknown_clauses_are_ignored_for_forward_compatibility() {
        let src = r#"
            (deflava-interface y
              :inputs ((:foo :type :string :brand-new-clause "ignored")))
        "#;
        let ifaces = interfaces_in_source(src).unwrap();
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].inputs.len(), 1);
    }

    #[test]
    fn skips_non_interface_forms() {
        let src = r#"
            (deflava-architecture demo
              :inputs ((:cidr "10.0.0.0/16"))
              :resources ((aws-vpc "x" :cidr-block "{cidr}")))
            (deflava-interface demo
              :inputs ((:cidr :type :cidr-block :required #t)))
        "#;
        let ifaces = interfaces_in_source(src).unwrap();
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].name, "demo");
    }

    #[test]
    fn unknown_primitive_type_surfaces_as_typed_error() {
        let src = r#"
            (deflava-interface z
              :inputs ((:x :type :not-a-real-type)))
        "#;
        let err = interfaces_in_source(src).unwrap_err();
        match err {
            InterfaceParseError::UnknownType(t) => assert_eq!(t, "not-a-real-type"),
            other => panic!("expected UnknownType, got {other:?}"),
        }
    }
}
