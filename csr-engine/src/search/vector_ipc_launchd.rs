//! The daemon's launchd job, read for the database it serves: how an MCP
//! server learns that a daemon is configured for its data dir (see
//! [`super::daemon_configured_for_database`]). Split out of `vector_ipc.rs`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

use super::{database_identity, heartbeat_path, ErrorCode, Reply, CONFIGURED_WITHIN};

pub(super) fn daemon_configured_with(
    db_path: &Path,
    home: Option<&Path>,
    disabled: bool,
    enabled: bool,
    ping: impl FnOnce(&Path) -> Result<Reply>,
) -> bool {
    if disabled {
        return false;
    }
    if enabled
        || home.is_some_and(|home| {
            launchd_database(home).is_some_and(|configured| {
                database_identity(&configured) == database_identity(db_path)
            })
        })
    {
        return true;
    }
    let Some(data_dir) = db_path.parent() else {
        return false;
    };
    let fresh = std::fs::metadata(heartbeat_path(data_dir))
        .and_then(|meta| meta.modified())
        .ok()
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
        })
        .is_some_and(|age| age < CONFIGURED_WITHIN);
    fresh
        && std::fs::read_to_string(heartbeat_path(data_dir))
            .ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            .is_some_and(|body| {
                body["database"].as_str() == Some(database_identity(db_path).as_str())
            })
        || matches!(
            ping(db_path),
            Ok(Reply::Pong
                | Reply::Error {
                    code: ErrorCode::Busy,
                    ..
                })
        )
}

fn plist_skip_misc(input: &mut &str) -> Option<()> {
    loop {
        *input = input.trim_start();
        if input.starts_with("<!--") {
            *input = &input[input.find("-->")? + 3..];
        } else if input.starts_with("<?") {
            *input = &input[input.find("?>")? + 2..];
        } else if input.starts_with("<!DOCTYPE") {
            *input = &input[input.find('>')? + 1..];
        } else {
            break;
        }
    }
    Some(())
}

/// Read launchd's XML property-list values without executing the job. Unknown
/// or malformed configuration is not evidence that this database has a daemon.
fn plist_value(input: &mut &str) -> Option<serde_json::Value> {
    use serde_json::Value;
    plist_skip_misc(input)?;
    let end = input.find('>')?;
    let tag = input.get(1..end)?;
    let empty = tag.ends_with('/');
    let name = tag.trim_end_matches('/').split_whitespace().next()?;
    *input = &input[end + 1..];
    let close = format!("</{name}>");
    let value = match name {
        "plist" if !empty => plist_value(input)?,
        "dict" => {
            let mut map = serde_json::Map::new();
            while !empty && !input.is_empty() {
                plist_skip_misc(input)?;
                if input.starts_with(&close) {
                    break;
                }
                let key = plist_value(input)?.as_str()?.to_owned();
                let value = plist_value(input)?;
                map.insert(key, value);
            }
            Value::Object(map)
        }
        "array" => {
            let mut values = Vec::new();
            while !empty && !input.is_empty() {
                plist_skip_misc(input)?;
                if input.starts_with(&close) {
                    break;
                }
                values.push(plist_value(input)?);
            }
            Value::Array(values)
        }
        "string" | "key" | "integer" | "real" | "data" | "date" => {
            let end = if empty { 0 } else { input.find(&close)? };
            let text = &input[..end];
            let mut decoded = String::new();
            let mut rest = text;
            while let Some(at) = rest.find('&') {
                decoded.push_str(&rest[..at]);
                let end = rest[at..].find(';')? + at;
                let entity = &rest[at + 1..end];
                decoded.push(match entity {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => {
                        let number = if let Some(hex) = entity.strip_prefix("#x") {
                            u32::from_str_radix(hex, 16).ok()?
                        } else {
                            entity.strip_prefix('#')?.parse().ok()?
                        };
                        char::from_u32(number)?
                    }
                });
                rest = &rest[end + 1..];
            }
            decoded.push_str(rest);
            *input = &input[end..];
            Value::String(decoded)
        }
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => return None,
    };
    if !empty {
        // Whitespace inside strings is data; only skip XML miscellany for containers.
        if matches!(name, "plist" | "dict" | "array") {
            plist_skip_misc(input)?;
        }
        *input = input.strip_prefix(&close)?;
    }
    Some(value)
}

pub(super) fn launchd_database(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(
        home.join("Library/LaunchAgents/com.ramakay.csr-engine.daemon.plist"),
    )
    .ok()?;
    let plist = plist_value(&mut text.as_str())?;
    plist.as_object()?;
    let args = plist
        .get("ProgramArguments")
        .and_then(serde_json::Value::as_array);
    let argument = args.and_then(|args| {
        args.iter().enumerate().find_map(|(i, arg)| {
            let arg = arg.as_str()?;
            if arg == "--db-path" {
                args.get(i + 1)?.as_str()
            } else {
                arg.strip_prefix("--db-path=")
            }
        })
    });
    let env = plist.get("EnvironmentVariables");
    let path = if let Some(path) = argument {
        PathBuf::from(path)
    } else {
        env.and_then(|env| env.get("HOME"))
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| home.to_path_buf())
            .join(".claude-self-reflect/csr-engine.db")
    };
    Some(if path.is_absolute() {
        path
    } else {
        Path::new(
            plist
                .get("WorkingDirectory")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("/"),
        )
        .join(path)
    })
}
