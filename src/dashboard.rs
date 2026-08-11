//! `(deflava-dashboard …)` — the dashboard document form.
//!
//! ## Why this is a converter and not a second parser
//!
//! The whole form body is ordinary nested data, so it is evaluated with
//! the same [`eval_value`](crate::eval::eval_value) every other clause
//! uses and then *type-converted* into a [`lava_core::Dashboard`]. That
//! keeps one value grammar in the crate rather than two, and it means
//! `{var}` interpolation, keyword-plist maps and arrays all work here
//! for free because they work everywhere.
//!
//! It also means this form is only expressible at all because
//! `eval_value` learned to nest: a dashboard is maps inside arrays
//! inside maps, four deep, and the previous grammar capped at two.
//!
//! ## Name
//!
//! `deflava-dashboard`, not `defpainel`. The `deflava-*` forms are a
//! closed enumerable set in plain English — architecture, resource,
//! interface, test, chart, module, provider, spec, import, cron, stack,
//! preset, function, serverless-function, template — and the naming laws
//! are explicit that a member of such a set matches the set's language,
//! because one Portuguese member reads as two families in one enum. The
//! Portuguese tier applies at the repo altitude (lava itself), not
//! inside the form namespace.

use lava_core::{
    Annotation, Dashboard, Datasource, DisplayMode, GraphMode, Panel, PanelKind, Presence, Query,
    QueryLang, Role, Row, Theme, Threshold, ThresholdConfig, ThresholdMode, TimeRange, Value,
    Variable, VariableKind,
};

use crate::eval::{eval_value, Env, EvalError};
use crate::sexpr::{parse_all, Sx};

/// Parse + evaluate a `(deflava-dashboard …)` form into a typed document.
///
/// # Errors
/// Returns [`EvalError`] when the source does not parse, carries no
/// dashboard form, or names a field with the wrong shape.
pub fn eval_dashboard(src: &str) -> Result<Dashboard, EvalError> {
    let forms = parse_all(src)?;
    let form = forms
        .iter()
        .find(|f| {
            f.as_list().and_then(|xs| xs.first().and_then(Sx::as_sym))
                == Some("deflava-dashboard")
        })
        .ok_or_else(|| EvalError::NotArchForm("no deflava-dashboard form".into()))?;
    let xs = form
        .as_list()
        .ok_or_else(|| EvalError::NotArchForm("not a list".into()))?;
    let name = xs.get(1).and_then(Sx::as_sym).unwrap_or("anonymous");

    let env = Env::new();
    let mut fields: Vec<(String, Value)> = Vec::new();
    let mut i = 2;
    while i + 1 < xs.len() {
        if let Some(k) = xs[i].as_kw() {
            fields.push((k.to_string(), eval_value(&xs[i + 1], &env)?));
        }
        i += 2;
    }
    let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v);

    let uid = get("uid")
        .and_then(as_str)
        .map_or_else(|| name.replace('_', "-"), ToString::to_string);
    let title = get("title")
        .and_then(as_str)
        .map_or_else(|| name.replace(['-', '_'], " "), ToString::to_string);

    let mut dash = Dashboard::new(uid, title);
    dash.description = get("description").and_then(as_str).map(ToString::to_string);
    if let Some(v) = get("tags") {
        dash.tags = as_list(v)
            .iter()
            .filter_map(|x| as_str(x).map(ToString::to_string))
            .collect();
    }
    if let Some(v) = get("refresh") {
        dash.refresh = as_str(v).map(ToString::to_string);
    }
    if let Some(v) = get("timezone").and_then(as_str) {
        dash.timezone = v.to_string();
    }
    if let Some(Value::Map(m)) = get("time") {
        dash.time = TimeRange {
            from: m.get("from").and_then(as_str).unwrap_or("now-1h").to_string(),
            to: m.get("to").and_then(as_str).unwrap_or("now").to_string(),
        };
    }
    if let Some(v) = get("editable") {
        dash.editable = matches!(v, Value::Bool(true)) || !matches!(v, Value::Bool(false));
    }

    for d in get("datasources").map(as_list).unwrap_or_default() {
        let Value::Map(m) = d else {
            return Err(EvalError::Type("datasource must be a keyword map"));
        };
        let uid = m
            .get("uid")
            .and_then(as_str)
            .ok_or(EvalError::Type("datasource :uid"))?;
        let wire = m.get("type").and_then(as_str).unwrap_or("prometheus");
        let lang = match m.get("lang").and_then(as_str).unwrap_or("promql") {
            "promql" => QueryLang::PromQl,
            "logsql" => QueryLang::LogsQl,
            "sql" => QueryLang::Sql,
            _ => return Err(EvalError::Type("datasource :lang must be promql|logsql|sql")),
        };
        dash.datasources
            .register(Datasource::new(uid, wire, lang));
    }

    for v in get("variables").map(as_list).unwrap_or_default() {
        let Value::Map(m) = v else {
            return Err(EvalError::Type("variable must be a keyword map"));
        };
        let name = m
            .get("name")
            .and_then(as_str)
            .ok_or(EvalError::Type("variable :name"))?;
        let kind = match m.get("kind").and_then(as_str).unwrap_or("query") {
            "query" => VariableKind::Query,
            "constant" => VariableKind::Constant,
            "custom" => VariableKind::Custom,
            "datasource" => VariableKind::Datasource,
            "textbox" => VariableKind::Textbox,
            "interval" => VariableKind::Interval,
            _ => return Err(EvalError::Type("variable :kind")),
        };
        let mut var = Variable::new(name, kind);
        var.label = m.get("label").and_then(as_str).map(ToString::to_string);
        var.datasource_uid = m.get("datasource").and_then(as_str).map(ToString::to_string);
        var.query = m.get("query").and_then(as_str).map(ToString::to_string);
        var.multi = matches!(m.get("multi"), Some(Value::Bool(true)));
        var.include_all = matches!(m.get("include-all"), Some(Value::Bool(true)));
        if let Some(o) = m.get("options") {
            var.options = as_list(o)
                .iter()
                .filter_map(|x| as_str(x).map(ToString::to_string))
                .collect();
        }
        dash.variables.push(var);
    }

    for a in get("annotations").map(as_list).unwrap_or_default() {
        let Value::Map(m) = a else {
            return Err(EvalError::Type("annotation must be a keyword map"));
        };
        dash.annotations.push(Annotation {
            name: m
                .get("name")
                .and_then(as_str)
                .ok_or(EvalError::Type("annotation :name"))?
                .to_string(),
            datasource_uid: m
                .get("datasource")
                .and_then(as_str)
                .ok_or(EvalError::Type("annotation :datasource"))?
                .to_string(),
            expr: m
                .get("expr")
                .and_then(as_str)
                .ok_or(EvalError::Type("annotation :expr"))?
                .to_string(),
            color: role(m.get("color").and_then(as_str).unwrap_or("info"))?,
            enable: !matches!(m.get("enable"), Some(Value::Bool(false))),
        });
    }

    for r in get("rows").map(as_list).unwrap_or_default() {
        let Value::Map(m) = r else {
            return Err(EvalError::Type("row must be a keyword map"));
        };
        let mut row = Row::new(
            m.get("title")
                .and_then(as_str)
                .ok_or(EvalError::Type("row :title"))?,
        );
        row.collapsed = matches!(m.get("collapsed"), Some(Value::Bool(true)));
        for p in m.get("panels").map(as_list).unwrap_or_default() {
            row.panels.push(panel_from(p)?);
        }
        dash.rows.push(row);
    }

    Ok(dash)
}

/// Render a `(deflava-dashboard …)` source straight to Grafana JSON.
///
/// # Errors
/// Returns [`EvalError`] on a parse or shape failure, and
/// [`EvalError::Type`] carrying the document error when the dashboard is
/// structurally valid but semantically rejected — a panel with no query,
/// an unregistered datasource, a query language mismatch.
pub fn render_dashboard_grafana_json(
    src: &str,
    theme: &Theme,
) -> Result<serde_json::Value, EvalError> {
    let dash = eval_dashboard(src)?;
    dash.render_grafana_json(theme)
        .map_err(|e| EvalError::Dashboard(e.to_string()))
}

fn panel_from(p: &Value) -> Result<Panel, EvalError> {
    let Value::Map(m) = p else {
        return Err(EvalError::Type("panel must be a keyword map"));
    };
    let id = m
        .get("id")
        .and_then(as_str)
        .ok_or(EvalError::Type("panel :id"))?;
    let kind = match m.get("kind").and_then(as_str).unwrap_or("timeseries") {
        "stat" => PanelKind::Stat,
        "timeseries" => PanelKind::TimeSeries,
        "gauge" => PanelKind::Gauge,
        "table" => PanelKind::Table,
        "heatmap" => PanelKind::Heatmap,
        "text" => PanelKind::Text,
        "pie" => PanelKind::Pie,
        other => {
            return Err(EvalError::Dashboard(format!(
                "panel {id}: unknown :kind {other:?} — expected one of stat, timeseries, gauge, table, heatmap, text, pie"
            )))
        }
    };
    let title = m
        .get("title")
        .and_then(as_str)
        .ok_or(EvalError::Type("panel :title"))?;
    let mut panel = Panel::new(id, kind, title);
    panel.description = m.get("description").and_then(as_str).map(ToString::to_string);
    panel.unit = m.get("unit").and_then(as_str).map(ToString::to_string);
    panel.min = m.get("min").and_then(as_f64);
    panel.max = m.get("max").and_then(as_f64);
    if let Some(w) = m.get("width").and_then(as_i64) {
        panel.width = u16::try_from(w).unwrap_or(panel.width);
    }
    if let Some(h) = m.get("height").and_then(as_i64) {
        panel.height = u16::try_from(h).unwrap_or(panel.height);
    }
    if let Some(d) = m.get("display").and_then(as_str) {
        panel.display_mode = match d {
            "none" => DisplayMode::None,
            "value" => DisplayMode::Value,
            "background" => DisplayMode::Background,
            _ => DisplayMode::Auto,
        };
    }
    if let Some(g) = m.get("graph").and_then(as_str) {
        panel.graph = match g {
            "none" => GraphMode::None,
            "area" => GraphMode::Area,
            _ => GraphMode::Auto,
        };
    }

    for (idx, q) in m
        .get("queries")
        .map(as_list)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let Value::Map(qm) = q else {
            return Err(EvalError::Type("query must be a keyword map"));
        };
        // refId defaults to A, B, C … so an author only names it when
        // something else references it.
        let default_ref = char::from(b'A' + u8::try_from(idx % 26).unwrap_or(0)).to_string();
        let reference = qm
            .get("ref")
            .and_then(as_str)
            .map_or(default_ref, ToString::to_string);
        let expr = qm
            .get("expr")
            .and_then(as_str)
            .ok_or(EvalError::Type("query :expr"))?;
        let ds = qm
            .get("datasource")
            .and_then(as_str)
            .ok_or(EvalError::Type("query :datasource"))?;
        let mut query = Query::new(reference, expr, ds);
        query.legend = qm.get("legend").and_then(as_str).map(ToString::to_string);
        query.instant = matches!(qm.get("instant"), Some(Value::Bool(true)));
        query.hide = matches!(qm.get("hide"), Some(Value::Bool(true)));
        if let Some(p) = qm.get("presence").and_then(as_str) {
            query.presence = match p {
                "event-driven" => Presence::EventDriven,
                "conditional" => Presence::Conditional,
                _ => Presence::Continuous,
            };
        }
        if let Some(Value::Map(alts)) = qm.get("alt-exprs") {
            for (target, v) in alts {
                if let Some(s) = as_str(v) {
                    query.alt_exprs.insert(target.clone(), s.to_string());
                }
            }
        }
        panel.queries.push(query);
    }

    let steps: Vec<Threshold> = m
        .get("thresholds")
        .map(as_list)
        .unwrap_or_default()
        .into_iter()
        .map(|t| {
            let Value::Map(tm) = t else {
                return Err(EvalError::Type("threshold must be a keyword map"));
            };
            Ok(Threshold {
                value: tm.get("value").and_then(as_f64),
                color: role(tm.get("color").and_then(as_str).unwrap_or("neutral"))?,
            })
        })
        .collect::<Result<_, _>>()?;
    if !steps.is_empty() {
        panel.thresholds = ThresholdConfig {
            mode: ThresholdMode::Absolute,
            steps,
        };
    }
    Ok(panel)
}

fn role(s: &str) -> Result<Role, EvalError> {
    Ok(match s {
        "neutral" => Role::Neutral,
        "primary" => Role::Primary,
        "info" => Role::Info,
        "success" => Role::Success,
        "warning" => Role::Warning,
        "danger" => Role::Danger,
        other => {
            return Err(EvalError::Dashboard(format!(
                "unknown colour role {other:?} — expected one of neutral, primary, info, success, warning, danger. Roles are semantic; a hex value is not accepted here by design."
            )))
        }
    })
}

fn as_str(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// A single map is treated as a one-element list, so an author writing
/// one row or one query does not have to wrap it.
fn as_list(v: &Value) -> Vec<&Value> {
    match v {
        Value::List(items) => items.iter().collect(),
        Value::Null => Vec::new(),
        single => vec![single],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
(deflava-dashboard fedramp-audit
  :uid "fedramp-audit"
  :title "FedRAMP · audit pipeline"
  :description "AU-5a / AU-9b / AU-12 coverage"
  :tags ("fedramp" "audit")
  :refresh "1m"
  :timezone "utc"
  :time (:from "now-6h" :to "now")
  :datasources ((:uid "mimir" :type "prometheus" :lang "promql")
                (:uid "clickhouse" :type "grafana-clickhouse-datasource" :lang "sql"))
  :rows ((:title "audit sink"
          :panels ((:id "sink_errors"
                    :kind "stat"
                    :title "Sink errors /5m"
                    :unit "short"
                    :description "AU-5a — a failing sink means audit records are being dropped."
                    :queries ((:ref "A"
                               :expr "sum(increase(vector_component_errors_total{component_kind=\"sink\"}[5m]))"
                               :datasource "mimir"
                               :presence "conditional"))
                    :thresholds ((:color "success") (:value 1 :color "danger")))
                   (:id "audit_rows"
                    :kind "table"
                    :title "Audit rows"
                    :queries ((:expr "SELECT count() FROM audit" :datasource "clickhouse")))))
         (:title "storage"
          :collapsed #t
          :panels ((:id "pvc_used"
                    :kind "gauge"
                    :title "Audit PVC used %"
                    :unit "percent"
                    :queries ((:expr "kubelet_volume_stats_used_bytes" :datasource "mimir"))))))
)
"#;

    #[test]
    fn a_dashboard_form_evaluates_to_a_typed_document() {
        let d = eval_dashboard(SRC).unwrap();
        assert_eq!(d.uid, "fedramp-audit");
        assert_eq!(d.title, "FedRAMP · audit pipeline");
        assert_eq!(d.tags, vec!["fedramp", "audit"]);
        assert_eq!(d.refresh.as_deref(), Some("1m"));
        assert_eq!(d.time.from, "now-6h");
        assert_eq!(d.datasources.0.len(), 2);
        assert_eq!(d.rows.len(), 2);
        assert_eq!(d.rows[0].panels.len(), 2);
        assert!(d.rows[1].collapsed);
        // refId defaults positionally so an author only names it when
        // something references it.
        assert_eq!(d.rows[0].panels[1].queries[0].reference, "A");
        assert_eq!(
            d.rows[0].panels[0].queries[0].presence,
            lava_core::Presence::Conditional
        );
    }

    #[test]
    fn this_form_is_only_expressible_because_eval_value_nests() {
        // The document is maps inside arrays inside maps, four deep:
        //   :rows → [ {:panels → [ {:queries → [ {…} ]} ]} ]
        // The previous grammar capped at depth 2 — a map value had to be
        // a string — so every one of these levels was a hard
        // EvalError::Type. This asserts the depth actually survived.
        let d = eval_dashboard(SRC).unwrap();
        let q = &d.rows[0].panels[0].queries[0];
        assert!(q.expr.contains("vector_component_errors_total"));
        assert_eq!(d.rows[0].panels[0].thresholds.steps.len(), 2);
    }

    #[test]
    fn it_renders_end_to_end_to_grafana_json() {
        let j = render_dashboard_grafana_json(SRC, &Theme::tundra()).unwrap();
        assert_eq!(j["schemaVersion"], 39);
        assert_eq!(j["uid"], "fedramp-audit");
        assert_eq!(j["refresh"], "1m");
        // 2 rows: row header + 2 panels expanded, then a collapsed row
        // whose child rides inside it.
        let panels = j["panels"].as_array().unwrap();
        assert_eq!(panels.len(), 4, "row + 2 panels + collapsed row");
        assert_eq!(panels[3]["type"], "row");
        assert_eq!(panels[3]["collapsed"], true);
        assert_eq!(panels[3]["panels"].as_array().unwrap().len(), 1);
        // The SQL datasource took the rawSql arm.
        assert_eq!(panels[2]["targets"][0]["rawSql"], "SELECT count() FROM audit");
    }

    #[test]
    fn an_unknown_panel_kind_names_the_alternatives() {
        let src = r#"(deflava-dashboard d :uid "d" :title "d"
            :datasources ((:uid "m" :type "prometheus" :lang "promql"))
            :rows ((:title "r" :panels ((:id "p" :kind "sparkline" :title "P"
                     :queries ((:expr "up" :datasource "m")))))))"#;
        let e = eval_dashboard(src).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("sparkline"), "{msg}");
        assert!(msg.contains("timeseries"), "must name the valid set: {msg}");
    }

    #[test]
    fn a_hex_colour_is_refused_because_roles_are_semantic() {
        // r##"…"## — a bare r#"…"# terminates on the `"#` inside the hex
        // literal this very test is about.
        let src = r##"(deflava-dashboard d :uid "d" :title "d"
            :datasources ((:uid "m" :type "prometheus" :lang "promql"))
            :rows ((:title "r" :panels ((:id "p" :kind "stat" :title "P"
                     :queries ((:expr "up" :datasource "m"))
                     :thresholds ((:color "#FF0000")))))))"##;
        let msg = eval_dashboard(src).unwrap_err().to_string();
        assert!(msg.contains("#FF0000"), "{msg}");
        assert!(msg.contains("semantic"), "must say why: {msg}");
    }

    #[test]
    fn a_document_level_rejection_surfaces_through_the_renderer() {
        // Structurally fine, semantically wrong: a logs query pointed at
        // the metrics datasource.
        let src = r#"(deflava-dashboard d :uid "d" :title "d"
            :datasources ((:uid "m" :type "prometheus" :lang "promql"))
            :rows ((:title "r" :panels ((:id "p" :kind "table" :title "P"
                     :queries ((:expr "{job=\"x\"} | json" :datasource "m")))))))"#;
        assert!(eval_dashboard(src).is_ok(), "the form itself is well-shaped");
        let msg = render_dashboard_grafana_json(src, &Theme::tundra())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("dashboard:"), "{msg}");
        assert!(msg.contains("PromQl") || msg.contains("LogsQl"), "{msg}");
    }

    #[test]
    fn rendering_the_same_source_twice_is_byte_identical() {
        let a = serde_json::to_string(
            &render_dashboard_grafana_json(SRC, &Theme::tundra()).unwrap(),
        )
        .unwrap();
        for _ in 0..8 {
            let b = serde_json::to_string(
                &render_dashboard_grafana_json(SRC, &Theme::tundra()).unwrap(),
            )
            .unwrap();
            assert_eq!(a, b);
        }
    }
}
