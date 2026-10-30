//! Read-only, administrator-allowlisted PostgreSQL imports. Never return driver errors: they may contain secrets.
use std::{collections::HashSet, io::{self, Write}, str::FromStr, time::Duration};

use anyhow::bail;
use axum::http::StatusCode;
use chrono::{DateTime, Datelike, Utc};
use futures::TryStreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{postgres::PgConnectOptions, ConnectOptions, Connection, Executor, PgConnection, Row};

use crate::{
    config::ResearchPostgresSource,
    research::{invalid, ResearchError, ResearchResult},
};

const MAX_ROWS: usize = 500_000;
const MAX_COLUMNS: usize = 128;
const MAX_CELLS: usize = 8_000_000;
const MAX_CSV_BYTES: usize = 32 * 1024 * 1024;

fn valid_identifier(value: &str) -> bool {
    // PostgreSQL identifiers are at most 63 UTF-8 bytes. Quotes/spaces/Unicode are legal;
    // every identifier is quoted below, never interpolated as SQL syntax.
    !value.trim().is_empty() && value.len() <= 63 && !value.chars().any(char::is_control)
}

pub(crate) fn validate_sources(sources: &[ResearchPostgresSource]) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    for source in sources {
        if source.id.is_empty() || source.id.len() > 128
            || !source.id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || !ids.insert(&source.id)
        {
            bail!("research.postgres_sources must have unique 1–128 character alphanumeric/underscore/hyphen IDs");
        }
        if source.name.trim().is_empty() || source.name.chars().count() > 200
            || source.description.chars().count() > 2000
        {
            bail!("research.postgres_sources requires a 1–200 character name and description of at most 2000 characters");
        }
        if source.dsn_env.is_empty() || source.dsn_env.len() > 128
            || !source.dsn_env.bytes().enumerate().all(|(index, c)| c == b'_' || c.is_ascii_alphabetic() || (index > 0 && c.is_ascii_digit()))
        {
            bail!("research.postgres_sources.dsn_env must name an environment variable, not contain a connection string");
        }
        if ![&source.schema, &source.table, &source.timestamp_column].into_iter().all(|s| valid_identifier(s))
            || source.columns.iter().any(|s| !valid_identifier(s))
        {
            bail!("research.postgres_sources identifiers must be nonempty, at most 63 UTF-8 bytes, and contain no control characters");
        }
        if !(2..=MAX_COLUMNS).contains(&source.columns.len())
            || source.columns.iter().collect::<HashSet<_>>().len() != source.columns.len()
            || source.columns.iter().filter(|c| *c == &source.timestamp_column).count() != 1
        {
            bail!("research.postgres_sources.columns must contain 2–128 unique columns including timestamp_column exactly once");
        }
        if !(2..=MAX_ROWS).contains(&source.max_rows) {
            bail!("research.postgres_sources.max_rows must be between 2 and 500000");
        }
        if !(1..=600).contains(&source.statement_timeout_seconds) {
            bail!("research.postgres_sources.statement_timeout_seconds must be between 1 and 600");
        }
    }
    Ok(())
}

pub(crate) fn public_source(source: &ResearchPostgresSource) -> Value {
    json!({
        "id": source.id, "name": source.name, "description": source.description,
        "timestamp_column": source.timestamp_column, "columns": source.columns,
        "max_rows": source.max_rows,
    })
}


pub(crate) fn find_source<'a>(sources: &'a [ResearchPostgresSource], id: &str) -> ResearchResult<&'a ResearchPostgresSource> {
    sources.iter().find(|source| source.id == id).ok_or_else(|| ResearchError(StatusCode::NOT_FOUND,
        "Unknown PostgreSQL source_id. Refresh the configured source list or ask an administrator to configure it.".into()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportBody {
    source_id: String,
    name: String,
    start: String,
    end: String,
}

pub(crate) struct ImportRequest {
    pub source_id: String,
    name: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

fn parse_time(value: &str) -> ResearchResult<DateTime<Utc>> {
    let time = DateTime::parse_from_rfc3339(value)
        .map_err(|_| invalid("start/end must be ISO8601 timestamps with an explicit timezone, for example 2026-01-01T00:00:00Z"))?
        .with_timezone(&Utc);
    if !(1..=9999).contains(&time.year()) || time.timestamp_subsec_nanos() >= 1_000_000_000
        || time.timestamp_subsec_nanos() % 1000 != 0
    {
        return Err(invalid("start/end must fall within UTC years 0001–9999 with at most microsecond precision and no leap seconds"));
    }
    Ok(time)
}

pub(crate) fn parse_request(payload: Value) -> ResearchResult<ImportRequest> {
    let body: ImportBody = serde_json::from_value(payload)
        .map_err(|_| invalid("PostgreSQL import requires only string fields source_id, name, start, and end"))?;
    if body.source_id.is_empty() || body.source_id.len() > 128
        || !body.source_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(invalid("Invalid PostgreSQL source_id"));
    }
    if body.name.trim().is_empty() || body.name.chars().count() > 200 {
        return Err(invalid("Dataset name must contain 1–200 characters"));
    }
    let start = parse_time(&body.start)?;
    let end = parse_time(&body.end)?;
    if start >= end {
        return Err(invalid("PostgreSQL import requires start < end; start is inclusive and end is exclusive"));
    }
    Ok(ImportRequest { source_id: body.source_id, name: body.name, start, end })
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn database_error(message: &'static str) -> ResearchError {
    ResearchError(StatusCode::BAD_GATEWAY, message.into())
}

fn overflow() -> ResearchError {
    ResearchError(StatusCode::PAYLOAD_TOO_LARGE,
        "PostgreSQL import exceeds the configured row limit, 8000000 cells, or 32 MiB CSV; select a shorter interval or ask an administrator for fewer columns. No partial dataset was imported.".into())
}

#[derive(Default)]
struct BoundedCsv(Vec<u8>);

impl Write for BoundedCsv {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_CSV_BYTES - self.0.len() {
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, "CSV exceeds 32 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn validate_column_types(source: &ResearchPostgresSource, types: &[(String, String, String)]) -> ResearchResult<()> {
    for column in &source.columns {
        let Some((_, kind, namespace)) = types.iter().find(|(name, _, _)| name == column) else {
            return Err(ResearchError(StatusCode::UNPROCESSABLE_ENTITY,
                "A configured PostgreSQL column or relation does not exist. Ask an administrator to check the source allowlist.".into()));
        };
        let supported = namespace == "pg_catalog" && if column == &source.timestamp_column {
            kind == "timestamptz"
        } else {
            matches!(kind.as_str(), "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" | "bool" | "text" | "varchar" | "bpchar")
        };
        if !supported {
            return Err(ResearchError(StatusCode::UNPROCESSABLE_ENTITY,
                "Unsupported PostgreSQL column type: timestamp_column must be timestamptz; other columns must be built-in numeric, boolean, or text types. Domains, arrays, and timezone-naive timestamps are not imported.".into()));
        }
    }
    Ok(())
}

async fn extract(source: &ResearchPostgresSource, request: ImportRequest) -> ResearchResult<Value> {
    // Resolve only at import time. Neither the DSN nor driver errors enter responses, audit, or logs.
    let dsn = std::env::var(&source.dsn_env).map_err(|_| ResearchError(StatusCode::SERVICE_UNAVAILABLE,
        "The configured PostgreSQL connection environment variable is unavailable. Ask an administrator to configure it.".into()))?;
    let options = PgConnectOptions::from_str(&dsn)
        .map_err(|_| ResearchError(StatusCode::SERVICE_UNAVAILABLE, "The administrator-configured PostgreSQL connection is invalid.".into()))?
        .disable_statement_logging();
    drop(dsn);
    // Retain the DSN's TLS mode/certificates unchanged; in particular never downgrade verify-full.
    let mut connection = tokio::time::timeout(Duration::from_secs(10.min(source.statement_timeout_seconds)), PgConnection::connect_with(&options))
        .await.map_err(|_| ResearchError(StatusCode::GATEWAY_TIMEOUT, "PostgreSQL connection timed out; no dataset was imported.".into()))?
        .map_err(|_| database_error("Cannot connect to the configured PostgreSQL source. Ask an administrator to check reachability, authentication, and TLS."))?;
    let mut transaction = connection.begin().await
        .map_err(|_| database_error("Cannot begin the PostgreSQL read-only import transaction."))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *transaction).await
        .map_err(|_| database_error("Cannot establish a read-only repeatable-read PostgreSQL transaction."))?;
    let timeout_ms = (source.statement_timeout_seconds * 1000).to_string();
    (&mut *transaction).execute(format!(
        "SET LOCAL statement_timeout = {timeout_ms}; SET LOCAL lock_timeout = {timeout_ms}; SET LOCAL idle_in_transaction_session_timeout = {timeout_ms}; SET LOCAL TIME ZONE 'UTC'; SET LOCAL DateStyle = 'ISO, YMD'; SET LOCAL extra_float_digits = 3; SET LOCAL search_path = pg_catalog"
    ).as_str()).await
        .map_err(|_| database_error("Cannot configure bounded PostgreSQL import settings."))?;
    let read_at = Utc::now();
    let relation = format!("{}.{}", quote_identifier(&source.schema), quote_identifier(&source.table));
    // Hold the relation's read lock before inspecting types, preventing schema changes between
    // validation and extraction. SET LOCAL above does not prematurely establish the MVCC snapshot.
    sqlx::query(&format!("LOCK TABLE {relation} IN ACCESS SHARE MODE"))
        .execute(&mut *transaction).await
        .map_err(|_| database_error("Cannot read-lock the configured PostgreSQL relation. Check read permissions or retry after concurrent schema maintenance."))?;
    let types = sqlx::query_as::<_, (String, String, String)>(
        "SELECT a.attname::text, t.typname::text, n.nspname::text FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_type t ON t.oid = a.atttypid JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace WHERE a.attrelid = pg_catalog.to_regclass($1) AND a.attname = ANY($2) AND a.attnum > 0 AND NOT a.attisdropped")
        .bind(&relation).bind(&source.columns).fetch_all(&mut *transaction).await
        .map_err(|_| database_error("Cannot inspect the configured PostgreSQL source columns. Ask an administrator to check the relation and read permissions."))?;
    validate_column_types(source, &types)?;

    let columns = source.columns.iter().map(|column| format!("{}::text", quote_identifier(column))).collect::<Vec<_>>().join(", ");
    let timestamp = quote_identifier(&source.timestamp_column);
    let row_limit = source.max_rows.min(MAX_CELLS / source.columns.len());
    // One text array per row bounds the entire wire row, including arbitrarily large text quality fields.
    // Server-side casts retain numeric digits, NULLs and microseconds without f64 conversion.
    // No DISTINCT, aggregation, interpolation or quality filtering occurs here.
    let query = format!("SELECT CASE WHEN pg_catalog.pg_column_size(cells) > {MAX_CSV_BYTES} THEN NULL ELSE cells END AS cells FROM (SELECT ARRAY[{columns}] AS cells FROM {relation} WHERE {timestamp} >= $1 AND {timestamp} < $2 ORDER BY {timestamp} LIMIT $3) AS selected");
    let mut writer = csv::WriterBuilder::new().from_writer(BoundedCsv::default());
    writer.write_record(&source.columns).map_err(|_| overflow())?;
    let mut row_count = 0;
    {
        let mut rows = sqlx::query(&query).bind(request.start).bind(request.end).bind((row_limit + 1) as i64)
            .fetch(&mut *transaction);
        while let Some(row) = rows.try_next().await
            .map_err(|_| database_error("PostgreSQL query failed or exceeded its statement timeout. No partial dataset was imported; narrow the interval or ask an administrator to check read permissions."))?
        {
            row_count += 1;
            if row_count > row_limit { return Err(overflow()); }
            let cells: Option<Vec<Option<String>>> = row.try_get("cells")
                .map_err(|_| database_error("Cannot decode the PostgreSQL result; no dataset was imported."))?;
            let cells = cells.ok_or_else(overflow)?;
            writer.write_record(cells.iter().map(|cell| cell.as_deref().unwrap_or("")))
                .map_err(|_| overflow())?;
        }
    }
    if row_count < 2 {
        return Err(ResearchError(StatusCode::UNPROCESSABLE_ENTITY,
            "The selected PostgreSQL interval contains fewer than two rows. Select a larger interval; no dataset was imported.".into()));
    }
    let csv = writer.into_inner().map_err(|_| overflow())?.0;
    let content = String::from_utf8(csv)
        .map_err(|_| database_error("PostgreSQL returned invalid UTF-8; no dataset was imported."))?;
    // There are no writes to commit. Closing a dedicated connection also ensures cancellation or
    // any early error drops the socket; an abandoned transaction is never returned to a pool.
    transaction.rollback().await
        .map_err(|_| database_error("Cannot finish the PostgreSQL read-only transaction; no dataset was imported."))?;
    let mut payload = json!({
        "format": "csv",
        "source": {
            "kind": "postgres", "source_id": source.id, "schema": source.schema, "table": source.table,
            "timestamp_column": source.timestamp_column, "columns": source.columns,
            "start": request.start.to_rfc3339(), "end": request.end.to_rfc3339(),
            "read_at": read_at.to_rfc3339(), "row_count": row_count,
        },
    });
    payload["name"] = Value::String(request.name);
    payload["content"] = Value::String(content);
    Ok(payload)
}

pub(crate) async fn import_payload(source: &ResearchPostgresSource, request: ImportRequest) -> ResearchResult<Value> {
    // Total deadline covers connection, metadata, query streaming, CSV encoding, and rollback.
    // Dropping extract owns/drops both transaction and connection, also on request cancellation.
    tokio::time::timeout(Duration::from_secs(source.statement_timeout_seconds), extract(source, request)).await
        .map_err(|_| ResearchError(StatusCode::GATEWAY_TIMEOUT,
            "PostgreSQL import timed out; no partial dataset was imported. Select a shorter interval.".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> ResearchPostgresSource {
        ResearchPostgresSource {
            id: "cement-kiln".into(), name: "水泥窯歷史資料".into(), description: "唯讀資料".into(),
            dsn_env: "AUTHRITY_RESEARCH_PG_DSN".into(), schema: "public".into(), table: "kiln_history".into(),
            timestamp_column: "time".into(), columns: vec!["time".into(), "pv".into(), "quality".into()],
            max_rows: 10000, statement_timeout_seconds: 30,
        }
    }

    #[test]
    fn configuration_rejects_duplicates_bad_identifiers_and_limits() {
        let valid = source();
        validate_sources(std::slice::from_ref(&valid)).unwrap();
        validate_sources(&[]).unwrap();
        assert!(validate_sources(&[valid.clone(), valid.clone()]).is_err());
        let mutations: [fn(&mut ResearchPostgresSource); 10] = [
            |s: &mut ResearchPostgresSource| s.columns.push("pv".into()),
            |s: &mut ResearchPostgresSource| { s.columns.remove(0); },
            |s: &mut ResearchPostgresSource| s.table = "bad\0table".into(),
            |s: &mut ResearchPostgresSource| s.table = "x".repeat(64),
            |s: &mut ResearchPostgresSource| s.dsn_env = "postgres://secret".into(),
            |s: &mut ResearchPostgresSource| s.max_rows = MAX_ROWS + 1,
            |s: &mut ResearchPostgresSource| s.max_rows = 0,
            |s: &mut ResearchPostgresSource| s.statement_timeout_seconds = 0,
            |s: &mut ResearchPostgresSource| s.statement_timeout_seconds = 601,
            |s: &mut ResearchPostgresSource| s.columns.extend((0..128).map(|i| format!("c{i}"))),
        ];
        for mutate in mutations {
            let mut invalid = valid.clone();
            mutate(&mut invalid);
            assert!(validate_sources(&[invalid]).is_err());
        }
        let mut quoted = valid;
        quoted.schema = "水泥 資料".into();
        quoted.table = "kiln\"; DROP TABLE history;--".into();
        validate_sources(&[quoted]).unwrap();
        assert_eq!(quote_identifier("a\".b"), "\"a\"\".b\"");
    }

    #[test]
    fn discovery_exposes_only_the_public_allowlist() {
        let value = public_source(&source());
        assert!(value.get("dsn_env").is_none());
        assert!(value.get("schema").is_none());
    }

    #[test]
    fn unknown_source_is_not_found_without_resolving_any_environment_variable() {
        assert_eq!(find_source(&[source()], "unknown").unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn request_requires_explicit_ordered_bounded_time_and_no_connection_overrides() {
        let valid = json!({"source_id":"cement-kiln", "name":"水泥窯", "start":"2026-01-01T08:00:00+08:00", "end":"2026-01-02T00:00:00Z"});
        let parsed = parse_request(valid.clone()).unwrap();
        assert_eq!(parsed.start.to_rfc3339(), "2026-01-01T00:00:00+00:00");
        for (key, value) in [
            ("start", "2026-01-01T00:00:00"), ("start", "2026-01-02T00:00:00Z"),
            ("start", "2026-01-03T00:00:00Z"), ("start", "0000-01-01T00:00:00Z"),
            ("start", "2026-01-01T00:00:00.0000001Z"), ("name", " "),
            ("dsn", "postgres://attacker"), ("sql", "SELECT 1"), ("source_id", "../other"),
        ] {
            let mut payload = valid.clone();
            payload[key] = json!(value);
            assert_eq!(parse_request(payload).err().unwrap().0, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn timestamp_without_timezone_missing_and_unsupported_columns_are_rejected() {
        let source = source();
        let mut types = vec![("time".into(), "timestamptz".into(), "pg_catalog".into()),
            ("pv".into(), "numeric".into(), "pg_catalog".into()), ("quality".into(), "text".into(), "pg_catalog".into())];
        validate_column_types(&source, &types).unwrap();
        types[0].1 = "timestamp".into();
        assert_eq!(validate_column_types(&source, &types).unwrap_err().0, StatusCode::UNPROCESSABLE_ENTITY);
        types[0].1 = "timestamptz".into();
        types[1].1 = "jsonb".into();
        assert!(validate_column_types(&source, &types).is_err());
        types.pop();
        assert!(validate_column_types(&source, &types).is_err());
    }

    #[test]
    fn csv_byte_budget_rejects_extra_data_without_truncating_existing_bytes() {
        let mut output = BoundedCsv(vec![0; MAX_CSV_BYTES]);
        assert!(output.write_all(b"x").is_err());
        assert_eq!(output.0.len(), MAX_CSV_BYTES);
    }
}
