//! In-memory evaluator for `.tlisp` architecture forms.
//!
//! Reads a `(deflava-architecture …)` form, runs the body with the
//! supplied input bindings, and produces a typed
//! [`lava_core::Architecture`] ready to render to terraform.json.
//!
//! Magma consumes this evaluator directly — no on-disk JSON dump
//! between authoring and plan/apply. The full pipeline:
//!
//! ```text
//! .tlisp source ──parse──► Sx
//!                    ──evaluate──► lava_core::Architecture
//!                              ──render_terraform_json──► serde_json::Value
//!                                          ──magma plan/apply──► cloud state
//! ```

use indexmap::IndexMap;
use lava_arch::Builder;
use lava_core::{Architecture, Resource, ResourceRef, Value};
use lava_schema::{Interface, SchemaError};
use std::collections::BTreeMap;
use thiserror::Error;

use crate::sexpr::{parse_all, Atom, Sx};

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("parse: {0}")]
    Parse(#[from] crate::sexpr::ParseError),
    #[error("expected deflava-architecture form, got {0}")]
    NotArchForm(String),
    #[error("missing :{0} clause")]
    MissingClause(&'static str),
    #[error("missing input binding for {0}")]
    MissingInput(String),
    #[error("unknown reference form: {0}")]
    BadRef(String),
    #[error("interpolation: unknown var `{0}`")]
    UnknownVar(String),
    #[error("type mismatch: expected {0}")]
    Type(&'static str),
    #[error("interface `{interface}` rejected {count} field(s): {first}")]
    Schema {
        interface: String,
        count: usize,
        first: String,
        errors: Vec<SchemaError>,
    },
}

/// Input binding for a single deflava-architecture evaluation.
#[derive(Debug, Clone, Default)]
pub struct InputBindings {
    scalars: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
}

impl InputBindings {
    pub fn new() -> Self { Self::default() }
    pub fn set_str(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.scalars.insert(k.into(), v.into());
    }
    pub fn set_list(&mut self, k: impl Into<String>, v: Vec<String>) {
        self.lists.insert(k.into(), v);
    }
}

/// Parse + evaluate a deflava-architecture form against a typed
/// [`Interface`]. Inputs are validated *before* evaluation; missing
/// required fields, unknown fields (closed-set interfaces only), and
/// per-field type mismatches all surface as
/// [`EvalError::Schema`] before any resource is constructed.
///
/// This is the GraphQL-equivalent gate: typed-contract failure at
/// compose-time, not at apply-time.
///
/// # Errors
/// [`EvalError::Schema`] when the bag violates the interface;
/// otherwise delegates to [`eval_architecture`].
pub fn eval_architecture_with_schema(
    src: &str,
    bindings: &InputBindings,
    iface: &Interface,
) -> Result<Architecture, EvalError> {
    // Project the InputBindings into the IndexMap<String,String> shape
    // lava-schema expects. Lists are joined with comma — lava-schema
    // currently treats lists as opaque strings; per-element typing
    // happens in lava-types::ListOf which we do not yet thread through
    // here. The scalar+list distinction is the InputBindings concern,
    // not the schema concern.
    let mut bag: IndexMap<String, String> = IndexMap::new();
    for (k, v) in &bindings.scalars {
        bag.insert(k.clone(), v.clone());
    }
    for (k, v) in &bindings.lists {
        bag.insert(k.clone(), v.join(","));
    }
    if let Err(errors) = iface.validate_inputs(&bag) {
        let first = errors
            .first()
            .map_or_else(|| "unknown".to_string(), std::string::ToString::to_string);
        return Err(EvalError::Schema {
            interface: iface.name.clone(),
            count: errors.len(),
            first,
            errors,
        });
    }
    eval_architecture(src, bindings)
}

/// Parse + evaluate a deflava-architecture form. Supplies any missing
/// inputs from the form's declared defaults.
pub fn eval_architecture(src: &str, bindings: &InputBindings) -> Result<Architecture, EvalError> {
    let forms = parse_all(src)?;
    // A .tlisp file may carry sibling declarations
    // (e.g. (deflava-interface …) next to (deflava-architecture …)).
    // Select the deflava-architecture form; the rest are typed
    // metadata consumed elsewhere.
    let form = forms
        .iter()
        .find(|f| {
            f.as_list()
                .and_then(|xs| xs.first().and_then(Sx::as_sym))
                == Some("deflava-architecture")
        })
        .ok_or_else(|| EvalError::NotArchForm("no deflava-architecture form".into()))?;
    let xs = form.as_list().ok_or_else(|| EvalError::NotArchForm("not a list".into()))?;
    let arch_name = xs.get(1).and_then(Sx::as_sym).unwrap_or("anonymous").to_string();

    // Walk :keyword clauses from position 2.
    let mut inputs_clause: Option<&Sx> = None;
    let mut resources_clause: Option<&Sx> = None;
    let mut i = 2;
    while i + 1 < xs.len() {
        let key = xs[i].as_kw();
        match key {
            Some("inputs")    => { inputs_clause    = Some(&xs[i + 1]); }
            Some("resources") => { resources_clause = Some(&xs[i + 1]); }
            // :result clause is for downstream consumers; not needed
            // to render terraform.json.
            Some("result") | Some(_) | None => {}
        }
        i += 2;
    }

    // Resolve inputs (defaults + user overrides).
    let mut env = Env::new();
    if let Some(inp) = inputs_clause {
        let pairs = inp.as_list().ok_or_else(|| EvalError::NotArchForm(":inputs not a list".into()))?;
        for pair in pairs {
            let plist = pair.as_list().ok_or_else(|| EvalError::NotArchForm("input not a list".into()))?;
            let key = plist.first().and_then(Sx::as_kw)
                .ok_or_else(|| EvalError::NotArchForm("input head not :kw".into()))?
                .to_string();
            // Default sits at plist[1].
            let default = plist.get(1);
            // User override wins.
            if let Some(s) = bindings.scalars.get(&key) {
                env.scalars.insert(key.clone(), s.clone());
            } else if let Some(l) = bindings.lists.get(&key) {
                env.lists.insert(key.clone(), l.clone());
            } else if let Some(d) = default {
                match d {
                    Sx::Atom(Atom::Str(s)) => { env.scalars.insert(key, s.clone()); }
                    Sx::Atom(Atom::Sym(s)) => { env.scalars.insert(key, s.clone()); }
                    Sx::List(items) => {
                        let mut out = Vec::new();
                        for it in items {
                            match it {
                                Sx::Atom(Atom::Str(s)) => out.push(s.clone()),
                                Sx::Atom(Atom::Sym(s)) => out.push(s.clone()),
                                _ => return Err(EvalError::Type("list of strings")),
                            }
                        }
                        env.lists.insert(key, out);
                    }
                    _ => return Err(EvalError::Type("string | list")),
                }
            } else {
                return Err(EvalError::MissingInput(key));
            }
        }
    }

    // Evaluate resources.
    let resources = resources_clause.ok_or(EvalError::MissingClause("resources"))?;
    let resource_list = resources.as_list().ok_or_else(|| EvalError::NotArchForm(":resources not a list".into()))?;

    let mut builder = Builder::new(arch_name);
    for r in resource_list {
        eval_resource(r, &env, &mut builder)?;
    }

    Ok(builder.finish())
}

#[derive(Default)]
struct Env {
    scalars: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
    // Loop-local single-element scalars (i, az).
    loop_scalars: BTreeMap<String, String>,
}

impl Env {
    fn new() -> Self { Self::default() }
    /// Resolve a `{var}` reference. Loop-local wins over global.
    fn get(&self, key: &str) -> Result<String, EvalError> {
        if let Some(s) = self.loop_scalars.get(key) { return Ok(s.clone()); }
        if let Some(s) = self.scalars.get(key) { return Ok(s.clone()); }
        Err(EvalError::UnknownVar(key.to_string()))
    }
}

fn eval_resource(r: &Sx, env: &Env, builder: &mut Builder) -> Result<(), EvalError> {
    let xs = r.as_list().ok_or_else(|| EvalError::NotArchForm("resource not a list".into()))?;
    let head = xs.first().and_then(Sx::as_sym)
        .ok_or_else(|| EvalError::NotArchForm("resource head not sym".into()))?;

    if head == "for-each" {
        // (for-each ((var ix) (enumerate :inputs.list)) <body-resource>)
        return eval_for_each(xs, env, builder);
    }

    // Generic resource: (aws-vpc "name" :attr v :attr v ...)
    let type_id = sym_to_type_id(head);
    let raw_name = xs.get(1).and_then(Sx::as_str)
        .ok_or_else(|| EvalError::NotArchForm("resource name not string".into()))?;
    let name = interp(raw_name, env)?;
    let mut attrs = IndexMap::new();
    let mut i = 2;
    while i + 1 < xs.len() {
        let key = xs[i].as_kw()
            .ok_or_else(|| EvalError::NotArchForm("attr key not :kw".into()))?
            .replace('-', "_");
        let val = eval_value(&xs[i + 1], env)?;
        attrs.insert(key, val);
        i += 2;
    }
    builder.add_resource(Resource {
        type_id,
        name,
        attributes: attrs,
        depends_on: vec![],
        provider: None,
        multiplicity: None,
    });
    Ok(())
}

fn eval_for_each(xs: &[Sx], env: &Env, builder: &mut Builder) -> Result<(), EvalError> {
    // xs = [ for-each, binding-form, body ]
    let binding_form = xs.get(1).and_then(Sx::as_list)
        .ok_or_else(|| EvalError::NotArchForm("for-each binding".into()))?;
    let vars = binding_form.first().and_then(Sx::as_list)
        .ok_or_else(|| EvalError::NotArchForm("for-each vars".into()))?;
    let iter_form = binding_form.get(1).and_then(Sx::as_list)
        .ok_or_else(|| EvalError::NotArchForm("for-each iter".into()))?;
    if iter_form.first().and_then(Sx::as_sym) != Some("enumerate") {
        return Err(EvalError::NotArchForm("for-each iter != enumerate".into()));
    }
    let source = iter_form.get(1).and_then(Sx::as_sym)
        .ok_or_else(|| EvalError::NotArchForm("enumerate source".into()))?;
    let items = env.lists.get(source)
        .ok_or_else(|| EvalError::UnknownVar(source.to_string()))?
        .clone();
    let body = xs.get(2).ok_or_else(|| EvalError::NotArchForm("for-each body".into()))?;

    let i_var = vars.first().and_then(Sx::as_sym).unwrap_or("i").to_string();
    let item_var = vars.get(1).and_then(Sx::as_sym).unwrap_or("item").to_string();

    for (i, item) in items.iter().enumerate() {
        let mut scoped = Env {
            scalars: env.scalars.clone(),
            lists: env.lists.clone(),
            loop_scalars: env.loop_scalars.clone(),
        };
        scoped.loop_scalars.insert(i_var.clone(), i.to_string());
        scoped.loop_scalars.insert(item_var.clone(), item.clone());
        eval_resource(body, &scoped, builder)?;
    }
    Ok(())
}

fn eval_value(s: &Sx, env: &Env) -> Result<Value, EvalError> {
    match s {
        Sx::Atom(Atom::Str(raw)) => Ok(Value::s(interp(raw, env)?)),
        Sx::Atom(Atom::Sym(s)) => {
            // Bare symbol — resolve as variable.
            Ok(Value::s(env.get(s)?))
        }
        Sx::Atom(Atom::Bool(b)) => Ok(Value::b(*b)),
        Sx::Atom(Atom::Int(n)) => Ok(Value::n(*n)),
        Sx::Atom(Atom::Kw(_)) => Err(EvalError::Type("value (not kw)")),
        Sx::List(items) => {
            // (ref aws-vpc "name" attr) → Value::Ref
            if let Some("ref") = items.first().and_then(Sx::as_sym) {
                let type_id = items.get(1).and_then(Sx::as_sym)
                    .ok_or_else(|| EvalError::BadRef("type".into()))?;
                let name_raw = items.get(2).and_then(Sx::as_str)
                    .ok_or_else(|| EvalError::BadRef("name".into()))?;
                let attr = items.get(3).and_then(Sx::as_sym)
                    .ok_or_else(|| EvalError::BadRef("attr".into()))?;
                let name = interp(name_raw, env)?;
                return Ok(Value::Ref(ResourceRef {
                    type_id: sym_to_type_id(type_id),
                    name,
                    attribute: attr.to_string(),
                }));
            }
            // Flat list of string/symbol atoms → JSON array of strings.
            // e.g. :bound-ips ("10.0.0.0/8" "10.10.0.0/16").
            if items.iter().all(|x| matches!(x, Sx::Atom(Atom::Str(_)) | Sx::Atom(Atom::Sym(_)))) {
                let arr: Result<Vec<Value>, EvalError> =
                    items.iter().map(|x| eval_value(x, env)).collect();
                return Ok(Value::arr(arr?));
            }
            // Otherwise: nested list of (Key Value) pairs → JSON map
            // (tag-pair shape).
            let mut map = serde_json::Map::new();
            for pair in items {
                let p = pair.as_list().ok_or(EvalError::Type("tag pair list"))?;
                let k_sym = p.first().and_then(Sx::as_sym)
                    .ok_or(EvalError::Type("tag key sym"))?;
                let v_raw = p.get(1).and_then(Sx::as_str)
                    .ok_or(EvalError::Type("tag val str"))?;
                map.insert(k_sym.to_string(), serde_json::Value::String(interp(v_raw, env)?));
            }
            Ok(Value::Json(serde_json::Value::Object(map)))
        }
    }
}

/// Substitute `{var}` and arithmetic-light `{var+N}` in template strings.
fn interp(template: &str, env: &Env) -> Result<String, EvalError> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // Find matching '}'.
            let end = bytes[i + 1..].iter().position(|c| *c == b'}')
                .map(|p| i + 1 + p);
            if let Some(j) = end {
                let expr = std::str::from_utf8(&bytes[i + 1..j])
                    .map_err(|_| EvalError::Type("utf8 in template"))?;
                out.push_str(&eval_template_expr(expr, env)?);
                i = j + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Ok(out)
}

/// Evaluate a template-string expression: `var` | `var+N` | `var-N`.
///
/// Plain-variable resolution is tried first so kebab-case variable
/// names like `name-prefix` are *not* mis-parsed as `name - prefix`.
/// Arithmetic only fires when the LHS resolves to an integer-typed
/// variable AND the RHS parses as an integer literal.
fn eval_template_expr(expr: &str, env: &Env) -> Result<String, EvalError> {
    let trimmed = expr.trim();
    if env.get(trimmed).is_ok() {
        return env.get(trimmed);
    }
    if let Some(idx) = trimmed.find('+') {
        let var = trimmed[..idx].trim();
        let off: i64 = trimmed[idx + 1..]
            .trim()
            .parse()
            .map_err(|_| EvalError::Type("int"))?;
        let v = env
            .get(var)?
            .parse::<i64>()
            .map_err(|_| EvalError::Type("int var"))?;
        return Ok((v + off).to_string());
    }
    if let Some(idx) = trimmed.rfind('-') {
        if idx > 0 {
            // Only treat as arithmetic if RHS is a pure integer literal AND
            // LHS resolves to an integer-typed loop / scalar variable.
            if let Ok(off) = trimmed[idx + 1..].trim().parse::<i64>() {
                let var = trimmed[..idx].trim();
                if let Ok(lhs) = env.get(var) {
                    if let Ok(v) = lhs.parse::<i64>() {
                        return Ok((v - off).to_string());
                    }
                }
            }
        }
    }
    env.get(trimmed)
}

/// Convert lisp-form symbol `aws-vpc` → terraform-form `aws_vpc`.
fn sym_to_type_id(s: &str) -> String {
    s.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_simple_var() {
        let mut env = Env::new();
        env.scalars.insert("name".into(), "main".into());
        assert_eq!(interp("{name}-vpc", &env).unwrap(), "main-vpc");
    }

    #[test]
    fn interpolates_arithmetic_var() {
        let mut env = Env::new();
        env.loop_scalars.insert("i".into(), "3".into());
        assert_eq!(interp("10.0.{i+10}.0/24", &env).unwrap(), "10.0.13.0/24");
    }

    #[test]
    fn sym_to_type_id_replaces_dashes() {
        assert_eq!(sym_to_type_id("aws-internet-gateway"), "aws_internet_gateway");
    }

    #[test]
    fn schema_validation_rejects_bad_input_before_evaluation() {
        use lava_schema::{Field, Interface};
        use lava_types::Type;

        let mut iface = Interface::new("demo-vpc");
        iface
            .inputs
            .insert("cidr".to_string(), Field::strict(Type::CidrBlock));

        let src = r#"
            (deflava-architecture demo-vpc
              :inputs ((:cidr "10.0.0.0/16"))
              :resources ((aws-vpc "demo" :cidr-block "{cidr}")))
        "#;

        let mut bindings = InputBindings::new();
        bindings.set_str("cidr", "not-a-cidr-at-all");

        let err = eval_architecture_with_schema(src, &bindings, &iface).unwrap_err();
        match err {
            EvalError::Schema { interface, count, .. } => {
                assert_eq!(interface, "demo-vpc");
                assert_eq!(count, 1);
            }
            other => panic!("expected EvalError::Schema, got {other:?}"),
        }
    }

    #[test]
    fn schema_validation_passes_with_valid_input() {
        use lava_schema::{Field, Interface};
        use lava_types::Type;

        let mut iface = Interface::new("demo-vpc");
        iface
            .inputs
            .insert("cidr".to_string(), Field::strict(Type::CidrBlock));

        let src = r#"
            (deflava-architecture demo-vpc
              :inputs ((:cidr "10.0.0.0/16"))
              :resources ((aws-vpc "demo" :cidr-block "{cidr}")))
        "#;

        let mut bindings = InputBindings::new();
        bindings.set_str("cidr", "10.42.0.0/16");

        let arch = eval_architecture_with_schema(src, &bindings, &iface).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert_eq!(json["resource"]["aws_vpc"]["demo"]["cidr_block"], "10.42.0.0/16");
    }
}
