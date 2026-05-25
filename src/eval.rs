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
use lava_core::{Architecture, Multiplicity, ProviderRef, Resource, ResourceRef, Value};
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
    let mut data_clause: Option<&Sx> = None;
    let mut outputs_clause: Option<&Sx> = None;
    let mut providers_clause: Option<&Sx> = None;
    let mut locals_clause: Option<&Sx> = None;
    let mut i = 2;
    while i + 1 < xs.len() {
        let key = xs[i].as_kw();
        match key {
            Some("inputs")    => { inputs_clause    = Some(&xs[i + 1]); }
            Some("resources") => { resources_clause = Some(&xs[i + 1]); }
            Some("data")      => { data_clause      = Some(&xs[i + 1]); }
            Some("outputs")   => { outputs_clause   = Some(&xs[i + 1]); }
            Some("providers") => { providers_clause = Some(&xs[i + 1]); }
            Some("locals")    => { locals_clause    = Some(&xs[i + 1]); }
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

    let mut arch = builder.finish();

    // :data — same shape as resources, lands in arch.data_sources.
    if let Some(data) = data_clause {
        let data_list = data
            .as_list()
            .ok_or_else(|| EvalError::NotArchForm(":data not a list".into()))?;
        for d in data_list {
            if let Some(r) = build_resource(d, &env)? {
                arch.data_sources.push(r);
            }
        }
    }

    // :outputs ( (:name (ref ...)) (:other (ref ...)) ... )
    if let Some(outs) = outputs_clause {
        let pairs = outs
            .as_list()
            .ok_or_else(|| EvalError::NotArchForm(":outputs not a list".into()))?;
        let mut j = 0;
        while j < pairs.len() {
            let k = pairs[j]
                .as_kw()
                .ok_or_else(|| EvalError::NotArchForm(":outputs head not :kw".into()))?
                .to_string();
            let v = pairs
                .get(j + 1)
                .ok_or_else(|| EvalError::NotArchForm(":outputs missing value".into()))?;
            arch.outputs.insert(k, eval_value(v, &env)?);
            j += 2;
        }
    }

    // :providers ( (aws :region "us-east-1") (aws :alias "west" :region "us-west-2") ... )
    if let Some(provs) = providers_clause {
        let list = provs
            .as_list()
            .ok_or_else(|| EvalError::NotArchForm(":providers not a list".into()))?;
        for p in list {
            let provider = build_provider(p, &env)?;
            arch.providers.push(provider);
        }
    }

    // :locals (:key value :key value ...)
    if let Some(locals) = locals_clause {
        let pairs = locals
            .as_list()
            .ok_or_else(|| EvalError::NotArchForm(":locals not a list".into()))?;
        let mut j = 0;
        while j < pairs.len() {
            let k = pairs[j]
                .as_kw()
                .ok_or_else(|| EvalError::NotArchForm(":locals head not :kw".into()))?
                .replace('-', "_");
            let v = pairs
                .get(j + 1)
                .ok_or_else(|| EvalError::NotArchForm(":locals missing value".into()))?;
            arch.locals.insert(k, eval_value(v, &env)?);
            j += 2;
        }
    }

    Ok(arch)
}

/// Build a typed Resource from a `(type-id "name" :attr v ...)` form.
/// Shared between :resources and :data evaluation.
///
/// Returns `Ok(None)` if the resource declares `:when #f` (or an
/// equivalent variable that resolves to a falsy value) — the caller
/// skips emitting it. Terraform's `count = condition ? 1 : 0`
/// equivalent in tlisp.
fn build_resource(r: &Sx, env: &Env) -> Result<Option<Resource>, EvalError> {
    let xs = r
        .as_list()
        .ok_or_else(|| EvalError::NotArchForm("resource not a list".into()))?;
    let head = xs
        .first()
        .and_then(Sx::as_sym)
        .ok_or_else(|| EvalError::NotArchForm("resource head not sym".into()))?;
    let type_id = sym_to_type_id(head);
    let raw_name = xs
        .get(1)
        .and_then(Sx::as_str)
        .ok_or_else(|| EvalError::NotArchForm("resource name not string".into()))?;
    let name = interp(raw_name, env)?;
    let mut attrs = IndexMap::new();
    let mut multiplicity: Option<Multiplicity> = None;
    let mut emit = true;
    let mut i = 2;
    while i + 1 < xs.len() {
        let raw_key = xs[i]
            .as_kw()
            .ok_or_else(|| EvalError::NotArchForm("attr key not :kw".into()))?;
        match raw_key {
            "when" => {
                emit = resolve_predicate(&xs[i + 1], env)?;
            }
            "count" => {
                let n = xs[i + 1]
                    .as_int()
                    .ok_or(EvalError::Type(":count expects integer"))?;
                multiplicity = Some(Multiplicity::Count(n));
            }
            "for-each" => {
                let map_form = xs[i + 1]
                    .as_list()
                    .ok_or(EvalError::Type(":for-each expects key/value pair list"))?;
                let mut m: IndexMap<String, Value> = IndexMap::new();
                let mut j = 0;
                while j + 1 < map_form.len() {
                    let mk = map_form[j]
                        .as_kw()
                        .or_else(|| map_form[j].as_str())
                        .ok_or(EvalError::Type(":for-each key not :kw|str"))?
                        .to_string();
                    let mv = eval_value(&map_form[j + 1], env)?;
                    m.insert(mk, mv);
                    j += 2;
                }
                multiplicity = Some(Multiplicity::ForEach(m));
            }
            _ => {
                let key = raw_key.replace('-', "_");
                let val = eval_value(&xs[i + 1], env)?;
                attrs.insert(key, val);
            }
        }
        i += 2;
    }
    if !emit {
        return Ok(None);
    }
    Ok(Some(Resource {
        type_id,
        name,
        attributes: attrs,
        depends_on: vec![],
        provider: None,
        multiplicity,
    }))
}

/// If `raw` is exactly the shape `{var}` (no other text + no arithmetic),
/// return the inner var name. Lets eval_value detect list-binding
/// interpolations and route them as typed arrays rather than scalars.
fn extract_full_var(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.is_empty() || inner.contains('{') || inner.contains('}') {
        return None;
    }
    if inner.contains('+') || inner.contains('-') {
        // Arithmetic / kebab — not a simple var lookup; let `interp`
        // handle it via the scalar path.
        return None;
    }
    Some(inner)
}

/// Resolve a `:when` value into a typed bool.
///
/// `#t`/`#f` literals are direct. A bare symbol or `{var}`-style
/// string is looked up in the env; truthy values: "true"/"#t"/"1";
/// falsy: "false"/"#f"/"0"/"". Anything else surfaces as a typed
/// error.
fn resolve_predicate(v: &Sx, env: &Env) -> Result<bool, EvalError> {
    match v {
        Sx::Atom(Atom::Bool(b)) => Ok(*b),
        Sx::Atom(Atom::Int(n)) => Ok(*n != 0),
        Sx::Atom(Atom::Sym(s) | Atom::Str(s)) => {
            let resolved = interp(s, env)?;
            match resolved.as_str() {
                "true" | "#t" | "1" => Ok(true),
                "false" | "#f" | "0" | "" => Ok(false),
                other => Err(EvalError::Type(match other {
                    _ => "boolean (true|false|#t|#f|1|0)",
                })),
            }
        }
        _ => Err(EvalError::Type("boolean")),
    }
}

/// Build a typed ProviderRef from a `(aws :region "us-east-1"
/// :alias "west" :source "hashicorp/aws")` form.
fn build_provider(p: &Sx, env: &Env) -> Result<ProviderRef, EvalError> {
    let xs = p
        .as_list()
        .ok_or_else(|| EvalError::NotArchForm("provider not a list".into()))?;
    let name = xs
        .first()
        .and_then(Sx::as_sym)
        .ok_or_else(|| EvalError::NotArchForm("provider head not sym".into()))?
        .to_string();
    let mut alias: Option<String> = None;
    let mut source = format!("hashicorp/{name}");
    let mut config: IndexMap<String, Value> = IndexMap::new();
    let mut i = 1;
    while i + 1 < xs.len() {
        let key = xs[i]
            .as_kw()
            .ok_or_else(|| EvalError::NotArchForm("provider attr key not :kw".into()))?;
        match key {
            "alias" => {
                alias = xs[i + 1].as_str().map(std::string::ToString::to_string);
            }
            "source" => {
                if let Some(s) = xs[i + 1].as_str() {
                    source = s.to_string();
                }
            }
            other => {
                let k = other.replace('-', "_");
                let v = eval_value(&xs[i + 1], env)?;
                config.insert(k, v);
            }
        }
        i += 2;
    }
    Ok(ProviderRef {
        source,
        name,
        alias,
        config,
    })
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
    if let Some(resource) = build_resource(r, env)? {
        builder.add_resource(resource);
    }
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
        Sx::Atom(Atom::Str(raw)) => {
            // Special-case `{var}` strings where var is a known list:
            // return the list as a typed array, not a comma-joined scalar.
            // This lets architectures pass list-bindings into resource
            // attributes without losing their list shape.
            if let Some(name) = extract_full_var(raw) {
                if let Some(items) = env.lists.get(name) {
                    let arr: Vec<Value> =
                        items.iter().cloned().map(Value::s).collect();
                    return Ok(Value::arr(arr));
                }
            }
            Ok(Value::s(interp(raw, env)?))
        }
        Sx::Atom(Atom::Sym(s)) => {
            // Bare symbol — resolve scalar first, then list. Lets
            // architectures write `:subnet-ids subnet-ids` and have
            // the list flow through as Value::arr.
            if let Ok(v) = env.get(s) {
                return Ok(Value::s(v));
            }
            if let Some(items) = env.lists.get(s) {
                let arr: Vec<Value> =
                    items.iter().cloned().map(Value::s).collect();
                return Ok(Value::arr(arr));
            }
            Err(EvalError::UnknownVar(s.to_string()))
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
    fn data_clause_renders_as_terraform_data_block() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-vpc "main" :cidr-block "10.0.0.0/16"))
              :data ((aws-ami "ubuntu" :most-recent #t :owners ("099720109477"))))
        "#;
        let arch = eval_architecture(src, &InputBindings::new()).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert_eq!(json["data"]["aws_ami"]["ubuntu"]["most_recent"], true);
        assert_eq!(json["data"]["aws_ami"]["ubuntu"]["owners"][0], "099720109477");
    }

    #[test]
    fn outputs_clause_renders_as_terraform_output_block() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-vpc "main" :cidr-block "10.0.0.0/16"))
              :outputs (:vpc-id (ref aws-vpc "main" id)
                        :vpc-cidr "10.0.0.0/16"))
        "#;
        let arch = eval_architecture(src, &InputBindings::new()).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert_eq!(json["output"]["vpc-id"]["value"], "${aws_vpc.main.id}");
        assert_eq!(json["output"]["vpc-cidr"]["value"], "10.0.0.0/16");
    }

    #[test]
    fn providers_clause_renders_with_typed_config() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-vpc "main" :cidr-block "10.0.0.0/16"))
              :providers ((aws :region "us-east-2" :profile "prod")
                          (aws :alias "west" :region "us-west-2")))
        "#;
        let arch = eval_architecture(src, &InputBindings::new()).unwrap();
        let json = arch.render_terraform_json().unwrap();
        // Two configs for "aws" → emitted as a JSON array.
        let arr = json["provider"]["aws"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["region"], "us-east-2");
        assert_eq!(arr[1]["alias"], "west");
        assert_eq!(arr[1]["region"], "us-west-2");
    }

    #[test]
    fn locals_clause_renders_under_top_level_locals() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-vpc "main" :cidr-block "10.0.0.0/16"))
              :locals (:env "prod" :retry-count 3))
        "#;
        let arch = eval_architecture(src, &InputBindings::new()).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert_eq!(json["locals"]["env"], "prod");
        assert_eq!(json["locals"]["retry_count"], 3);
    }

    #[test]
    fn resource_count_meta_arg_renders_as_terraform_count() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-instance "node" :count 3 :ami "ami-12345" :instance-type "t3.micro")))
        "#;
        let arch = eval_architecture(src, &InputBindings::new()).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert_eq!(json["resource"]["aws_instance"]["node"]["count"], 3);
        assert_eq!(json["resource"]["aws_instance"]["node"]["ami"], "ami-12345");
    }

    #[test]
    fn resource_for_each_meta_arg_renders_as_terraform_for_each() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-s3-bucket "data"
                            :for-each (:logs "tag-logs" :metrics "tag-metrics")
                            :bucket "demo")))
        "#;
        let arch = eval_architecture(src, &InputBindings::new()).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert_eq!(json["resource"]["aws_s3_bucket"]["data"]["for_each"]["logs"], "tag-logs");
        assert_eq!(json["resource"]["aws_s3_bucket"]["data"]["for_each"]["metrics"], "tag-metrics");
    }

    #[test]
    fn when_false_skips_resource_entirely() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-vpc "main" :when #t :cidr-block "10.0.0.0/16")
                          (aws-subnet "skip" :when #f :cidr-block "10.0.1.0/24")))
        "#;
        let arch = eval_architecture(src, &InputBindings::new()).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert!(json["resource"]["aws_vpc"]["main"].is_object());
        assert!(json["resource"]["aws_subnet"].is_null(),
            "aws_subnet should be omitted entirely when :when #f");
    }

    #[test]
    fn when_predicate_resolves_variable_bindings() {
        let src = r#"
            (deflava-architecture x
              :inputs ((:enable "true"))
              :resources ((aws-vpc "main" :when "{enable}" :cidr-block "10.0.0.0/16")))
        "#;
        let mut b = InputBindings::new();
        b.set_str("enable", "true");
        let arch = eval_architecture(src, &b).unwrap();
        assert!(arch
            .render_terraform_json()
            .unwrap()
            ["resource"]["aws_vpc"]["main"]
            .is_object());

        let mut b = InputBindings::new();
        b.set_str("enable", "false");
        let arch = eval_architecture(src, &b).unwrap();
        let json = arch.render_terraform_json().unwrap();
        assert!(
            json["resource"].is_null()
                || json["resource"]["aws_vpc"].is_null(),
            "vpc should be omitted when :enable=false"
        );
    }

    #[test]
    fn when_with_unparseable_value_surfaces_typed_error() {
        let src = r#"
            (deflava-architecture x
              :inputs ()
              :resources ((aws-vpc "main" :when "maybe" :cidr-block "10.0.0.0/16")))
        "#;
        let err = eval_architecture(src, &InputBindings::new()).unwrap_err();
        assert!(matches!(err, EvalError::Type(_)));
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
