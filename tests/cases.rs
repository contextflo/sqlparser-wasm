use serde::Deserialize;
use sqlparser_wasm::{tables, Options};

#[derive(Deserialize)]
struct Case {
    name: String,
    dialect: String,
    sql: String,
    #[serde(default)]
    options: Option<serde_json::Value>,
    tables: Option<Vec<String>>,
    // Expected error prefix; "" means any error
    error: Option<String>,
}

#[test]
fn sample_cases() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("cases.json")).expect("cases.json parses");
    let mut failures = Vec::new();

    for case in &cases {
        let options: Options = match &case.options {
            None => Options::default(),
            Some(value) => serde_json::from_value(value.clone()).expect("options parse"),
        };
        let result = tables(&case.sql, &case.dialect, &options);
        match (&case.tables, &case.error, result) {
            (Some(expected), None, Ok(found)) => {
                let names: Vec<String> = found.into_iter().map(|table| table.name).collect();
                if &names != expected {
                    failures.push(format!("{}: expected {:?}, got {:?}", case.name, expected, names));
                }
            }
            (None, Some(prefix), Err(reason)) => {
                if !reason.starts_with(prefix.as_str()) {
                    failures.push(format!("{}: expected error starting {:?}, got {:?}", case.name, prefix, reason));
                }
            }
            (_, _, Ok(found)) => failures.push(format!("{}: expected an error, got {:?}", case.name, found)),
            (_, _, Err(reason)) => failures.push(format!("{}: unexpected error {:?}", case.name, reason)),
        }
    }

    assert!(failures.is_empty(), "{} of {} cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
}
