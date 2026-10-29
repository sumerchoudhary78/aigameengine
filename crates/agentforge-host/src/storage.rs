//! File persistence for projects, replays and rendered output.
//!
//! Writes are atomic where the platform allows: data goes to a temporary file in the
//! destination directory, is flushed to disk, and is then renamed over the destination, so
//! readers see either the old file or the new one, never a torn write. Reads are bounded
//! and validated before anything in a session changes.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use agentforge_core::project::{
    PROJECT_FORMAT, PROJECT_FORMAT_VERSION, Project, ProjectDocument, ProjectError,
};
use agentforge_core::replay::{REPLAY_FORMAT, REPLAY_FORMAT_VERSION, Replay};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Largest file this module will read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// A storage failure.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The operating system refused a read or write.
    #[error("could not {action} {}: {source}", path.display())]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The file exceeds [`MAX_FILE_BYTES`].
    #[error("{} is {size} bytes; the limit is {MAX_FILE_BYTES}", path.display())]
    TooLarge {
        /// The file involved.
        path: PathBuf,
        /// Its size.
        size: u64,
    },
    /// The file is not valid JSON of the expected shape.
    #[error("{} is not a valid {what}: {message}", path.display())]
    Malformed {
        /// The file involved.
        path: PathBuf,
        /// What it should have been.
        what: &'static str,
        /// Parser message (with line and column).
        message: String,
    },
    /// The file declares a format or version this build does not support. Checked before
    /// the full structure is parsed, so files from newer versions get this precise error.
    #[error(
        "{}: {what} declares {}; this engine supports format {expected_format:?} version {expected_version}",
        path.display(),
        describe_header(found_format.as_deref(), *found_version)
    )]
    Incompatible {
        /// The file involved.
        path: PathBuf,
        /// What it should have been.
        what: &'static str,
        /// Declared `format`, if any.
        found_format: Option<String>,
        /// Declared `format_version`, if any.
        found_version: Option<u64>,
        /// Supported format.
        expected_format: &'static str,
        /// Supported version.
        expected_version: u32,
    },
    /// The file parsed but its project content is invalid or incompatible.
    #[error("{}: {source}", path.display())]
    InvalidProject {
        /// The file involved.
        path: PathBuf,
        /// What is wrong.
        source: ProjectError,
    },
}

fn describe_header(format: Option<&str>, version: Option<u64>) -> String {
    let format = format.map_or_else(|| "no format".to_owned(), |f| format!("format {f:?}"));
    let version = version.map_or_else(
        || "no format_version".to_owned(),
        |v| format!("version {v}"),
    );
    format!("{format} {version}")
}

/// Reads and validates a project file.
pub fn load_project(path: &Path) -> Result<Project, StorageError> {
    let document: ProjectDocument =
        read_versioned(path, "project file", PROJECT_FORMAT, PROJECT_FORMAT_VERSION)?;
    Project::from_document(document).map_err(|source| StorageError::InvalidProject {
        path: path.to_owned(),
        source,
    })
}

/// Writes a project file atomically; returns the number of bytes written.
pub fn save_project(path: &Path, project: &Project) -> Result<u64, StorageError> {
    write_json(path, &project.to_document())
}

/// Reads a replay file. The format header is checked here; the hash-encoding version is
/// checked by verification.
pub fn load_replay(path: &Path) -> Result<Replay, StorageError> {
    read_versioned(path, "replay file", REPLAY_FORMAT, REPLAY_FORMAT_VERSION)
}

/// Writes a replay file atomically; returns the number of bytes written.
pub fn save_replay(path: &Path, replay: &Replay) -> Result<u64, StorageError> {
    write_json(path, replay)
}

/// Serializes `value` as readable JSON (see [`readable_json`]) with a trailing newline and
/// writes it atomically.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<u64, StorageError> {
    let value = serde_json::to_value(value).map_err(|error| StorageError::Malformed {
        path: path.to_owned(),
        what: "serializable value",
        message: error.to_string(),
    })?;
    let mut text = readable_json(&value);
    text.push('\n');
    write_atomic(path, text.as_bytes())?;
    Ok(text.len() as u64)
}

/// Widest line [`readable_json`] keeps on one line.
const READABLE_WIDTH: usize = 160;

/// Formats JSON for people and language models alike: any object or array whose one-line
/// form fits in [`READABLE_WIDTH`] columns stays on one line, larger ones are expanded one
/// member per line. A scene file therefore lists one entity per line instead of ten.
/// Key order follows the Rust struct definitions.
pub fn readable_json(value: &Value) -> String {
    let mut out = String::new();
    write_readable(&mut out, value, 0);
    out
}

fn write_readable(out: &mut String, value: &Value, indent: usize) {
    let inline = inline_json(value);
    let expandable = match value {
        Value::Array(items) => !items.is_empty(),
        Value::Object(members) => !members.is_empty(),
        _ => false,
    };
    if !expandable || indent + inline.len() <= READABLE_WIDTH {
        out.push_str(&inline);
        return;
    }
    let pad = " ".repeat(indent + 2);
    match value {
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                out.push_str(if index == 0 { "\n" } else { ",\n" });
                out.push_str(&pad);
                write_readable(out, item, indent + 2);
            }
        }
        Value::Object(members) => {
            out.push('{');
            for (index, (key, member)) in members.iter().enumerate() {
                out.push_str(if index == 0 { "\n" } else { ",\n" });
                out.push_str(&pad);
                out.push_str(&Value::String(key.clone()).to_string());
                out.push_str(": ");
                write_readable(out, member, indent + 2);
            }
        }
        _ => {}
    }
    out.push('\n');
    out.push_str(&" ".repeat(indent));
    out.push(if value.is_array() { ']' } else { '}' });
}

/// One-line JSON with a space after each `,` and `:`.
fn inline_json(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(inline_json).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(members) => {
            let parts: Vec<String> = members
                .iter()
                .map(|(key, member)| {
                    format!("{}: {}", Value::String(key.clone()), inline_json(member))
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        scalar => scalar.to_string(),
    }
}

/// Writes `bytes` to `path` atomically (temporary file in the same directory, fsync,
/// rename, then a best-effort fsync of the directory).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let io_error = |action, source| StorageError::Io {
        action,
        path: path.to_owned(),
        source,
    };
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    // Temporary files are created owner-only; without this the rename would leave the
    // destination with mode 0600. Keep an existing file's permissions, otherwise use the
    // conventional 0644 (further restricted by the process umask).
    let permissions = match std::fs::metadata(path) {
        Ok(existing) => Some(existing.permissions()),
        Err(_) => default_permissions(),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(".agentforge-").suffix(".tmp");
    if let Some(permissions) = permissions {
        builder.permissions(permissions);
    }
    let mut temporary = builder
        .tempfile_in(directory)
        .map_err(|e| io_error("create a temporary file next to", e))?;
    temporary
        .write_all(bytes)
        .map_err(|e| io_error("write a temporary file for", e))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| io_error("flush a temporary file for", e))?;
    temporary
        .persist(path)
        .map_err(|e| io_error("replace", e.error))?;
    #[cfg(unix)]
    if let Ok(handle) = File::open(directory) {
        // Persisting the rename itself; failure here does not undo the completed write.
        let _ = handle.sync_all();
    }
    Ok(())
}

/// Permissions for a newly created file: 0644 on Unix (further restricted by the umask);
/// elsewhere the platform default.
#[cfg(unix)]
fn default_permissions() -> Option<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;
    Some(std::fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
fn default_permissions() -> Option<std::fs::Permissions> {
    None
}

/// Reads a JSON file, checks its `format`/`format_version` header, then parses it strictly.
fn read_versioned<T: DeserializeOwned>(
    path: &Path,
    what: &'static str,
    expected_format: &'static str,
    expected_version: u32,
) -> Result<T, StorageError> {
    let value = read_value(path, what)?;
    let found_format = value
        .get("format")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let found_version = value.get("format_version").and_then(Value::as_u64);
    if found_format.as_deref() != Some(expected_format)
        || found_version != Some(u64::from(expected_version))
    {
        return Err(StorageError::Incompatible {
            path: path.to_owned(),
            what,
            found_format,
            found_version,
            expected_format,
            expected_version,
        });
    }
    serde_json::from_value(value).map_err(|error| StorageError::Malformed {
        path: path.to_owned(),
        what,
        message: error.to_string(),
    })
}

fn read_value(path: &Path, what: &'static str) -> Result<Value, StorageError> {
    let io_error = |action, source| StorageError::Io {
        action,
        path: path.to_owned(),
        source,
    };
    let file = File::open(path).map_err(|e| io_error("open", e))?;
    let size = file.metadata().map_err(|e| io_error("inspect", e))?.len();
    if size > MAX_FILE_BYTES {
        return Err(StorageError::TooLarge {
            path: path.to_owned(),
            size,
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read", e))?;
    serde_json::from_slice(&bytes).map_err(|error| StorageError::Malformed {
        path: path.to_owned(),
        what,
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentforge_core::project::Name;

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temporary_files() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("project.agentforge.json");
        let project = Project::new(Name::new("first").expect("valid"));
        save_project(&path, &project).expect("first save");
        let renamed = Project::new(Name::new("second").expect("valid"));
        save_project(&path, &renamed).expect("second save");
        assert_eq!(load_project(&path).expect("loads"), renamed);
        let leftovers: Vec<_> = std::fs::read_dir(directory.path())
            .expect("listable")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }

    /// Regression: files written through a temporary file were left with mode 0600.
    #[cfg(unix)]
    #[test]
    fn saved_files_get_shareable_permissions_and_keep_existing_ones() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("shared.json");
        write_atomic(&path, b"{}").expect("write");
        let mode = std::fs::metadata(&path)
            .expect("exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode & 0o044,
            0o044,
            "group/other must be able to read, got {mode:o}"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        write_atomic(&path, b"{}").expect("rewrite");
        let mode = std::fs::metadata(&path)
            .expect("exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "an existing file's permissions are preserved");
    }

    #[test]
    fn readable_json_keeps_small_values_inline_and_round_trips() {
        let value = serde_json::json!({
            "name": "demo",
            "entities": (0..20).map(|i| serde_json::json!({"id": i, "kind": {"type": "wall"}})).collect::<Vec<_>>(),
            "empty": [],
        });
        let text = readable_json(&value);
        assert!(
            text.contains("\n    {\"id\": 3, \"kind\": {\"type\": \"wall\"}}"),
            "{text}"
        );
        assert!(text.contains("\"empty\": []"));
        let parsed: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(parsed, value);
    }

    #[test]
    fn malformed_and_missing_files_are_reported_with_context() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("broken.json");
        std::fs::write(&path, b"{ not json").expect("writable");
        let error = load_project(&path).expect_err("malformed");
        assert!(matches!(error, StorageError::Malformed { .. }), "{error}");
        let missing = load_project(&directory.path().join("missing.json")).expect_err("missing");
        assert!(matches!(missing, StorageError::Io { action: "open", .. }));
    }
}
