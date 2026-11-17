// Runs the same sample cases against the built WASM package: node tests/smoke.mjs
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const { tables } = require('../pkg/sqlparser_wasm.js')
const cases = JSON.parse(readFileSync(new URL('./cases.json', import.meta.url), 'utf8'))

let failed = 0
for (const testCase of cases) {
  const result = JSON.parse(tables(testCase.sql, testCase.dialect, testCase.options ? JSON.stringify(testCase.options) : undefined))
  const pass = testCase.error === undefined
    ? result.ok && JSON.stringify(result.tables.map((table) => table.name)) === JSON.stringify(testCase.tables)
    : !result.ok && result.reason.startsWith(testCase.error)
  if (!pass) {
    failed++
    console.log(`FAIL ${testCase.name}: ${JSON.stringify(result)}`)
  }
}

const started = performance.now()
const runs = 2000
for (let index = 0; index < runs; index++) {
  tables(cases[index % cases.length].sql, 'snowflake')
}
const perQuery = (performance.now() - started) / runs

console.log(`${cases.length - failed}/${cases.length} cases pass, ${perQuery.toFixed(3)} ms per query`)
process.exit(failed === 0 ? 0 : 1)
