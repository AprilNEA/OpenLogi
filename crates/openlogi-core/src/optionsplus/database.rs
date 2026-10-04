//! Read committed WAL data through SQLite, never a raw main-file copy.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OpenFlags, limits::Limit};

use super::{ImportError, MAX_DOCUMENT_BYTES};

/// Read the one current settings row in a read transaction. Missing, locked,
/// oversized, malformed, or ambiguous databases are errors, not empty settings.
/// The source is opened read-only, including its committed WAL contents.
pub fn read_database(path: &Path) -> Result<Vec<u8>, ImportError> {
    // SQLite must reopen the original path to find its WAL. Reject special-file
    // inputs first; this does not prevent an uncooperative concurrent rename
    // between this check and SQLite's own open.
    let _source = crate::file_input::FileInput::Source
        .open(path)
        .map_err(|error| {
            ImportError::Invalid(format!("cannot read database {}: {error}", path.display()))
        })?;
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_millis(250))?;
    connection.pragma_update(None, "query_only", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, 9 * 1024 * 1024)?;
    let started = Instant::now();
    connection.progress_handler(
        1000,
        Some(move || started.elapsed() > Duration::from_secs(2)),
    )?;
    let transaction = connection.transaction()?;
    let (kind, definition): (String, String) = transaction.query_row(
        "SELECT type, sql FROM sqlite_schema WHERE name = 'data'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if kind != "table"
        || !definition
            .split_ascii_whitespace()
            .take(2)
            .map(str::to_ascii_uppercase)
            .eq(["CREATE", "TABLE"])
    {
        return Err(ImportError::Invalid(
            "data must be an ordinary table".into(),
        ));
    }
    let mut statement = transaction.prepare("SELECT length(file) FROM data LIMIT 2")?;
    let lengths: Vec<i64> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let [length] = lengths.as_slice() else {
        return Err(ImportError::Invalid(
            "expected exactly one current settings row".into(),
        ));
    };
    if *length < 1 || usize::try_from(*length).map_or(true, |length| length > MAX_DOCUMENT_BYTES) {
        return Err(ImportError::Invalid(
            "invalid settings document size".into(),
        ));
    }
    let bytes =
        transaction.query_row("SELECT file FROM data", [], |row| row.get::<_, Vec<u8>>(0))?;
    Ok(bytes)
}
