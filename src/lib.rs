//! Which tables does a SQL query read?
//!
//! Parses with Apache DataFusion's sqlparser-rs and walks the tree. Fails closed: anything the
//! walker does not recognise (a statement that is not a query, an unknown FROM item kind, a table
//! function not on the allowlist, a dynamic table name) is an error, never a partial list.

use std::collections::HashSet;
use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    Expr, FunctionArguments, ObjectName, ObjectNamePart, Query, Statement, TableFactor, Visit,
    Visitor,
};
use sqlparser::dialect::dialect_from_str;
use sqlparser::parser::Parser;
use wasm_bindgen::prelude::*;

/// Table functions that only reshape their arguments and read no table themselves.
const DEFAULT_TABLE_FUNCTIONS: &[&str] = &[
    "flatten",
    "generator",
    "split_to_table",
    "generate_series",
    "unnest",
];

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    /// Prepended to names written without a database
    pub default_database: Option<String>,
    /// Prepended to names written without a schema
    pub default_schema: Option<String>,
    /// Table functions to allow on top of the defaults (matched on the last name part,
    /// case-insensitive)
    #[serde(default)]
    pub allow_table_functions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableRef {
    /// Parts joined with dots, case-folded the way the dialect resolves names
    pub name: String,
    /// The same parts, unjoined, for names whose parts contain dots
    pub parts: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Output {
    Ok { ok: bool, tables: Vec<TableRef> },
    Err { ok: bool, reason: String },
}

/// JS entry point. Returns `{"ok":true,"tables":[...]}` or `{"ok":false,"reason":"..."}`.
#[wasm_bindgen(js_name = tables)]
pub fn tables_json(sql: &str, dialect: &str, options_json: Option<String>) -> String {
    let output = match parse_options(options_json) {
        Err(reason) => Output::Err { ok: false, reason },
        Ok(options) => match tables(sql, dialect, &options) {
            Ok(tables) => Output::Ok { ok: true, tables },
            Err(reason) => Output::Err { ok: false, reason },
        },
    };
    serde_json::to_string(&output).unwrap_or_else(|_| r#"{"ok":false,"reason":"Could not serialize the result"}"#.to_string())
}

/// Dialect names this build accepts
#[wasm_bindgen]
pub fn dialects() -> Vec<String> {
    [
        "ansi", "bigquery", "clickhouse", "databricks", "duckdb", "generic", "hive", "mssql",
        "mysql", "oracle", "postgres", "redshift", "snowflake", "sqlite", "teradata",
    ]
    .iter()
    .map(|name| name.to_string())
    .collect()
}

fn parse_options(options_json: Option<String>) -> Result<Options, String> {
    match options_json {
        None => Ok(Options::default()),
        Some(json) => serde_json::from_str(&json).map_err(|error| format!("Invalid options: {error}")),
    }
}

pub fn tables(sql: &str, dialect_name: &str, options: &Options) -> Result<Vec<TableRef>, String> {
    let dialect = dialect_from_str(dialect_name).ok_or_else(|| format!("Unknown dialect: {dialect_name}"))?;
    let statements = Parser::parse_sql(dialect.as_ref(), sql).map_err(|error| format!("Could not parse: {error}"))?;

    let statement = match statements.as_slice() {
        [single] => single,
        [] => return Err("No statement".to_string()),
        _ => return Err("More than one statement".to_string()),
    };
    if !matches!(statement, Statement::Query(_)) {
        return Err("Not a query".to_string());
    }

    let mut allowed_functions: HashSet<String> = DEFAULT_TABLE_FUNCTIONS.iter().map(|name| name.to_string()).collect();
    allowed_functions.extend(options.allow_table_functions.iter().map(|name| name.to_lowercase()));

    let mut walker = Walker {
        folding: Folding::for_dialect(dialect_name),
        allowed_functions,
        frames: Vec::new(),
        found: Vec::new(),
    };
    if let ControlFlow::Break(reason) = statement.visit(&mut walker) {
        return Err(reason);
    }

    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for parts in walker.found {
        let parts = qualify(parts, options);
        let name = parts.join(".");
        if seen.insert(name.clone()) {
            result.push(TableRef { name, parts });
        }
    }
    Ok(result)
}

fn qualify(parts: Vec<String>, options: &Options) -> Vec<String> {
    let mut prefix = Vec::new();
    if parts.len() < 3 {
        if let Some(database) = &options.default_database {
            prefix.push(database.clone());
        }
    }
    if parts.len() < 2 {
        if let Some(schema) = &options.default_schema {
            prefix.push(schema.clone());
        }
    }
    prefix.extend(parts);
    prefix
}

/// How unquoted identifiers resolve
#[derive(Clone, Copy)]
enum Folding {
    Upper,
    Lower,
    AsWritten,
}

impl Folding {
    fn for_dialect(dialect_name: &str) -> Folding {
        match dialect_name.to_lowercase().as_str() {
            "snowflake" | "oracle" => Folding::Upper,
            "postgres" | "postgresql" | "redshift" | "duckdb" => Folding::Lower,
            _ => Folding::AsWritten,
        }
    }
}

/// One query's WITH clause, as seen from inside that query
struct Frame {
    names: Vec<String>,
    recursive: bool,
    // Identifies which CTE body is being visited (the visitor hands out the same references)
    bodies: Vec<*const Query>,
    // How many of `names` are in scope right now: all of them in the query body, only the
    // earlier ones inside a non-recursive CTE body
    visible: usize,
}

struct Walker {
    folding: Folding,
    allowed_functions: HashSet<String>,
    frames: Vec<Frame>,
    found: Vec<Vec<String>>,
}

impl Walker {
    fn fold(&self, value: &str, quoted: bool) -> String {
        if quoted {
            return value.to_string();
        }
        match self.folding {
            Folding::Upper => value.to_uppercase(),
            Folding::Lower => value.to_lowercase(),
            Folding::AsWritten => value.to_string(),
        }
    }

    fn name_parts(&self, name: &ObjectName) -> Result<Vec<String>, String> {
        name.0
            .iter()
            .map(|part| match part {
                ObjectNamePart::Identifier(ident) => Ok(self.fold(&ident.value, ident.quote_style.is_some())),
                ObjectNamePart::Function(function) => Err(format!("Dynamic table name: {}", function.name.value)),
            })
            .collect()
    }

    fn is_cte(&self, parts: &[String]) -> bool {
        let [name] = parts else { return false };
        self.frames.iter().rev().any(|frame| frame.names[..frame.visible].contains(name))
    }

    fn check_function(&self, name: &ObjectName) -> ControlFlow<String> {
        let last = name.0.last().and_then(|part| part.as_ident()).map(|ident| ident.value.to_lowercase());
        match last {
            Some(function) if name.0.len() == 1 && self.allowed_functions.contains(&function) => ControlFlow::Continue(()),
            _ => ControlFlow::Break(format!("Table function not allowed: {name}")),
        }
    }
}

impl Visitor for Walker {
    type Break = String;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<String> {
        let pointer: *const Query = query;
        if let Some(frame) = self.frames.last_mut() {
            if let Some(index) = frame.bodies.iter().position(|body| *body == pointer) {
                frame.visible = if frame.recursive { frame.names.len() } else { index };
            }
        }

        let frame = match &query.with {
            None => Frame { names: Vec::new(), recursive: false, bodies: Vec::new(), visible: 0 },
            Some(with) => {
                let names: Vec<String> = with
                    .cte_tables
                    .iter()
                    .map(|cte| self.fold(&cte.alias.name.value, cte.alias.name.quote_style.is_some()))
                    .collect();
                Frame {
                    visible: names.len(),
                    names,
                    recursive: with.recursive,
                    bodies: with.cte_tables.iter().map(|cte| &*cte.query as *const Query).collect(),
                }
            }
        };
        self.frames.push(frame);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, query: &Query) -> ControlFlow<String> {
        self.frames.pop();
        let pointer: *const Query = query;
        if let Some(frame) = self.frames.last_mut() {
            if frame.bodies.contains(&pointer) {
                frame.visible = frame.names.len();
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, table_factor: &TableFactor) -> ControlFlow<String> {
        match table_factor {
            // `FROM f(...)`: a table-valued function, not a table
            TableFactor::Table { name, args: Some(_), .. } => self.check_function(name),
            TableFactor::Table { name, args: None, .. } | TableFactor::SemanticView { name, .. } => {
                match self.name_parts(name) {
                    Err(reason) => ControlFlow::Break(reason),
                    Ok(parts) => {
                        if !self.is_cte(&parts) {
                            self.found.push(parts);
                        }
                        ControlFlow::Continue(())
                    }
                }
            }
            TableFactor::Function { name, .. } => self.check_function(name),
            TableFactor::TableFunction { expr, .. } => match expr {
                Expr::Function(function) if !matches!(function.args, FunctionArguments::Subquery(_)) => {
                    self.check_function(&function.name)
                }
                _ => ControlFlow::Break(format!("Unsupported TABLE(...) argument: {expr}")),
            },
            // The visitor walks into these and reaches the tables they wrap
            TableFactor::Derived { .. }
            | TableFactor::NestedJoin { .. }
            | TableFactor::Pivot { .. }
            | TableFactor::Unpivot { .. }
            | TableFactor::UNNEST { .. } => ControlFlow::Continue(()),
            other => ControlFlow::Break(format!("Unsupported FROM item: {other}")),
        }
    }
}
