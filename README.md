# sqlparser-wasm

Which tables does a SQL query read? A WebAssembly build of [Apache DataFusion's sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs) with one job: take a query, return the tables it reads, or refuse.

This is a thin wrapper, not a fork. All parsing is upstream's, pinned to `sqlparser = "=0.64.0"`. Parser fixes go upstream.

We built it for access control in front of AI agents: an agent writes SQL, you check the tables against your rules before the query reaches the warehouse. That use case shapes one rule above all others.

## It fails closed

If the walker meets anything it does not recognise, it returns an error, never a partial list. A parser that confidently returns 7 of 8 tables is a leak; one that says "I can't tell" is safe. Rejected:

- anything that is not a single query (`EXPLAIN`, `SHOW`, `DESCRIBE`, writes, multiple statements)
- FROM item kinds outside the allowlist below
- table functions not on the allowlist (`TABLE(my_udtf(...))`, `INFORMATION_SCHEMA.QUERY_HISTORY_BY_USER(...)`)
- dynamic names (`IDENTIFIER('...')`)

| FROM item | Handling |
| --- | --- |
| Table, semantic view | Reported by name |
| Subquery, nested join, `PIVOT`, `UNPIVOT`, `UNNEST` | Walked into |
| `FLATTEN`, `GENERATOR`, `SPLIT_TO_TABLE`, `GENERATE_SERIES`, `UNNEST` | Allowed, arguments walked |
| Anything else | Error |

Tables inside expressions (`IN (SELECT ...)`, `EXISTS`, scalar subqueries) are found anywhere in the tree. CTE names are scoped exactly: `WITH orders AS (SELECT * FROM orders)` reports the real `orders`.

## Usage

```js
const { tables } = require('./pkg/sqlparser_wasm.js')

JSON.parse(tables('SELECT * FROM orders o JOIN crm.customers c ON c.id = o.customer_id', 'snowflake',
  JSON.stringify({ defaultDatabase: 'ANALYTICS', defaultSchema: 'PUBLIC' })))
// { ok: true, tables: [
//   { name: 'ANALYTICS.PUBLIC.ORDERS', parts: ['ANALYTICS', 'PUBLIC', 'ORDERS'] },
//   { name: 'ANALYTICS.CRM.CUSTOMERS', parts: ['ANALYTICS', 'CRM', 'CUSTOMERS'] } ] }

JSON.parse(tables('EXPLAIN SELECT COUNT(*) FROM orders', 'snowflake'))
// { ok: false, reason: 'Not a query' }
```

Options (all optional):

- `defaultDatabase`, `defaultSchema`: prepended to names written without them
- `allowTableFunctions`: extra unqualified table functions to allow

Names are case-folded the way the dialect resolves them: Snowflake uppercases unquoted names, Postgres, Redshift and DuckDB lowercase them, quoted names keep their case. `parts` is there for names whose parts contain dots.

Dialects: `ansi`, `bigquery`, `clickhouse`, `databricks`, `duckdb`, `generic`, `hive`, `mssql`, `mysql`, `oracle`, `postgres`, `redshift`, `snowflake`, `sqlite`, `teradata`. Snowflake is the one we test hardest.

## What it does not do

- **Views are reported by name.** It does not know what a view reads; that needs the warehouse.
- **Scalar functions are not checked.** A SQL UDF that reads a table internally is invisible here.
- **No row or column rules.** Tables only.

## How well it works

On 5,303 real Snowflake queries written by AI agents (casts, `col:path` JSON access, `QUALIFY`, `LATERAL FLATTEN`, `* EXCLUDE`, ...), sqlparser-rs failed to parse 1 (`DESCRIBE USER`) and agreed with sqlglot's table lists on 99.9%. For comparison, node-sql-parser failed on 6.1%. We don't publish that corpus; `tests/cases.json` holds samples of each construct written from scratch.

## Build and test

```sh
cargo test                                   # sample cases, native
wasm-pack build --target nodejs --release    # -> pkg/
node tests/smoke.mjs                         # same cases against the WASM build
```

## License

Apache-2.0, same as upstream. See `LICENSE` and `NOTICE`.
