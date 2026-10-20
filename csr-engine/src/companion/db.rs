use super::paths::{git, Paths};
use crate::storage::Storage;
use rusqlite::{params_from_iter, types::ValueRef};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Db {
    storage: Storage,
    shifts: std::cell::RefCell<BTreeMap<std::path::PathBuf, (std::time::Instant, Vec<Value>)>>,

    #[cfg(test)]
    pub(super) queries: std::sync::atomic::AtomicUsize,
}
impl Db {
    pub fn open(paths: &Paths) -> anyhow::Result<Self> {
        let storage = Storage::open_read_only(&paths.db)?;
        storage.with_connection(|con| {
            con.set_prepared_statement_cache_capacity(128);
            let _: i64 = con.pragma_query_value(None, "schema_version", |row| row.get(0))?;
            // One frame runs about 21 distinct statements; rusqlite keeps 16 by default.
            con.set_prepared_statement_cache_capacity(64);
            Ok(())
        })?;
        Ok(Self {
            storage,
            shifts: Default::default(),
            #[cfg(test)]
            queries: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    pub fn learnt_counts(&self, project: &str, sid: &str, now: f64) -> Vec<(usize, &'static str)> {
        let Some(end) = chrono::DateTime::from_timestamp(now as i64, 0) else {
            return vec![];
        };
        let feeds = self.storage.learnt_feeds(
            project,
            sid,
            &end.to_rfc3339(),
            crate::storage::recap_feeds::dream_consumption_mode(),
        );
        let mut out = vec![];
        if feeds.notes_total > 0 && !feeds.notes.is_empty() && feeds.notes_date.is_some() {
            out.push((feeds.notes_total, "notes kept"));
        }
        if feeds.confirmed_total > 0 && !feeds.confirmed.is_empty() {
            out.push((feeds.confirmed_total, "findings confirmed"));
        }
        let retired = feeds.verdicts.iter().map(|v| v.count).sum::<usize>();
        if retired > 0 {
            out.push((retired, "facts retired"));
        }
        out
    }
    pub fn query(&self, sql: &str, params: &[Value]) -> Vec<Vec<Value>> {
        self.try_query(sql, params).unwrap_or_default()
    }
    pub fn try_query(&self, sql: &str, params: &[Value]) -> anyhow::Result<Vec<Vec<Value>>> {
        #[cfg(test)]
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.storage.with_connection(|con| {
            let values: Vec<rusqlite::types::Value> = params
                .iter()
                .map(|v| match v {
                    Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                    Value::Number(n) if n.is_i64() => {
                        rusqlite::types::Value::Integer(n.as_i64().unwrap())
                    }
                    Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.)),
                    _ => rusqlite::types::Value::Null,
                })
                .collect();
            let mut stmt = con.prepare_cached(sql)?;
            let n = stmt.column_count();
            let rows = stmt
                .query_map(params_from_iter(values), |row| {
                    (0..n)
                        .map(|i| {
                            Ok(match row.get_ref(i)? {
                                ValueRef::Null => Value::Null,
                                ValueRef::Integer(n) => json!(n),
                                ValueRef::Real(n) => json!(n),
                                ValueRef::Text(s) => {
                                    json!(String::from_utf8_lossy(s))
                                }
                                ValueRef::Blob(_) => Value::Null,
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
    pub fn scalar(&self, sql: &str, params: &[Value]) -> Value {
        self.query(sql, params)
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .unwrap_or(Value::Null)
    }
    pub fn pane_root(&self, repo: &Path) -> Option<std::path::PathBuf> {
        git(repo, &["rev-parse", "--show-toplevel"])
            .map(Into::into)
            .or_else(|| {
                let root = repo.canonicalize().ok()?;
                let project = super::paths::csr_project(root.to_str()?)?;
                (self.scalar(
                    "select 1 from chunks where project_name=? limit 1",
                    &[json!(project)],
                ) == json!(1))
                .then_some(root)
            })
    }
    pub fn shifts(&self, root: &Path, limit: usize) -> Vec<Value> {
        if let Some((at, rows)) = self.shifts.borrow().get(root) {
            if at.elapsed().as_secs() < 60 {
                return rows.iter().take(limit).cloned().collect();
            }
        }
        let rows = self.read_shifts(root, None);
        let result = rows.iter().take(limit).cloned().collect();
        self.shifts
            .borrow_mut()
            .insert(root.into(), (std::time::Instant::now(), rows));
        result
    }
    pub fn shifts_for_file(&self, root: &Path, file: &Path) -> Vec<Value> {
        if let Some((at, rows)) = self.shifts.borrow().get(root) {
            if at.elapsed().as_secs() < 60 {
                return rows
                    .iter()
                    .filter(|s| s["path"] == file.to_string_lossy().as_ref())
                    .cloned()
                    .collect();
            }
        }
        self.read_shifts(root, Some(file))
    }
    fn read_shifts(&self, root: &Path, file: Option<&Path>) -> Vec<Value> {
        if git(root, &["rev-parse", "--show-toplevel"]).is_none() {
            return vec![];
        }
        let floor = self.scalar(
            "select coalesce((select cast(value as integer) from meta where \
                key='witness_verdicts_v2_floor'),0)",
            &[],
        );
        let (predicate, target) = if let Some(file) = file {
            ("l.file=?2", file.to_string_lossy().into_owned())
        } else {
            ("l.file like ?2", format!("{}/%", root.display()))
        };
        let sql = format!(
            "select l.file,l.symbol,v.verdict,v.receipt_oid,\
                coalesce(v.receipt_committed_at,v.created_at),v.id,v.witness_id \
                from witness_verdicts v join witness_ledger l on \
                l.id=v.witness_id where v.id>?1 and v.receipt_oid is not null \
                and trim(v.receipt_oid)!='' and {predicate} and v.id=(select \
                max(v2.id) from witness_verdicts v2 where \
                v2.witness_id=v.witness_id)"
        );
        let rows = self.query(&sql, &[floor, json!(target)]);
        let mut grouped = BTreeMap::<String, Value>::new();
        for r in rows {
            let path = r[0].as_str().unwrap_or("");
            let rel = path
                .strip_prefix(&format!("{}/", root.display()))
                .unwrap_or(path);
            if rel.contains(".claude/worktrees/") || rel.contains(".worktrees/") {
                continue;
            }
            let key = json!([rel, r[1], r[2], r[3]]).to_string();
            let verb = match r[2].as_str().unwrap_or("") {
                "superseded_by" => "changed",
                "anchor_obsolete" => "removed",
                "anchor_reinstated" => "came back",
                s => s,
            };
            let date = r[4]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(10)
                .collect::<String>();
            let e = grouped.entry(key).or_insert_with(|| {
                json!({
                    "file": rel,
                    "path": path,
                    "symbol": r[1],
                    "verdict": r[2],
                    "verb": verb,
                    "oid": r[3],
                    "date": date,
                    "at": super::paths::epoch(r[4].as_str().unwrap_or("")),
                    "id": r[5],
                    "wids": [],
                })
            });
            e["wids"].as_array_mut().unwrap().push(r[6].clone());
            if r[5].as_i64() > e["id"].as_i64() {
                e["id"] = r[5].clone();
            }
        }
        let mut rows: Vec<_> = grouped.into_values().collect();
        let oids: Vec<_> = rows
            .iter()
            .map(|s| s["oid"].as_str().unwrap_or("").to_owned())
            .collect();
        let mut commits = super::sources::commit_infos(root, &oids);
        let mut symbols = crate::dream::symbol_cli::Session::new();
        // A merge receipt may only carry another commit's change. Ask the engine in process.
        rows.retain_mut(|s| {
            let oid = s["oid"].as_str().unwrap_or("").to_owned();
            let merging = commits.get(&oid).is_some_and(|r| r.0);
            if merging {
                let mut verdicts = Vec::new();
                for wid in s["wids"].as_array().unwrap() {
                    let from = self.scalar(
                        "select at_oid from witness_ledger where id=?",
                        std::slice::from_ref(wid),
                    );
                    let Some(from) = from.as_str() else {
                        return false;
                    };
                    let req = crate::dream::symbol_cli::ChangeRequest {
                        repo: root.to_str().unwrap_or(""),
                        file: s["file"].as_str().unwrap_or(""),
                        symbol: s["symbol"].as_str(),
                        oid: &oid,
                        from: Some(from),
                        summary: false,
                    };
                    let Ok(answer) = crate::dream::symbol_cli::symbol_change_in(&mut symbols, &req)
                    else {
                        return false;
                    };
                    let author = if answer["authored"] == true {
                        json!([oid, s["date"], s["at"]])
                    } else if answer["authored"] == false && answer["author"].is_string() {
                        json!([
                            answer["author"],
                            answer["author_committed_at"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(10)
                                .collect::<String>(),
                            super::paths::epoch(
                                answer["author_committed_at"].as_str().unwrap_or("")
                            ),
                        ])
                    } else {
                        return false;
                    };
                    verdicts.push(author);
                }
                if verdicts.is_empty() || verdicts.iter().any(|v| v != &verdicts[0]) {
                    return false;
                }
                s["oid"] = verdicts[0][0].clone();
                s["date"] = verdicts[0][1].clone();
                s["at"] = verdicts[0][2].clone();
            }
            true
        });
        let unknown: Vec<_> = rows
            .iter()
            .map(|s| s["oid"].as_str().unwrap_or(""))
            .filter(|oid| !commits.contains_key(*oid))
            .map(str::to_owned)
            .collect();
        commits.extend(super::sources::commit_infos(root, &unknown));
        for s in &mut rows {
            s["subject"] = json!(commits
                .get(s["oid"].as_str().unwrap_or(""))
                .map(|r| r.1.as_str())
                .unwrap_or(""));
        }
        let mut settled = BTreeMap::<String, Value>::new();
        for s in rows {
            let key = json!([s["file"], s["symbol"], s["verdict"], s["oid"]]).to_string();
            let e = settled.entry(key).or_insert_with(|| {
                let mut value = s.clone();
                value["wids"] = json!([]);
                value
            });
            for wid in s["wids"].as_array().unwrap() {
                if !e["wids"].as_array().unwrap().contains(wid) {
                    e["wids"].as_array_mut().unwrap().push(wid.clone());
                }
            }
            if s["id"].as_i64() > e["id"].as_i64() {
                e["id"] = s["id"].clone();
            }
        }
        let mut rows: Vec<_> = settled.into_values().collect();
        rows.sort_by(|a, b| {
            (b["date"].as_str(), b["id"].as_i64()).cmp(&(a["date"].as_str(), a["id"].as_i64()))
        });
        rows
    }
}
impl Db {
    pub fn embeddings(&self, sid: &str, part: Option<&str>) -> Vec<Vec<f64>> {
        self.storage
            .with_connection(|con| {
                let sql = if part.is_some() {
                    "select e.embedding from chunks c join chunk_embeddings e on e.chunk_id=c.id \
                 where c.conversation_id=?1 and instr(c.content,?2)>0 limit 1"
                } else {
                    "select e.embedding from chunks c join chunk_embeddings e on e.chunk_id=c.id \
                 where c.conversation_id=?1 order by c.timestamp desc limit 8"
                };
                let mut statement = con.prepare_cached(sql)?;
                let params: Vec<&dyn rusqlite::ToSql> = if let Some(ref part) = part {
                    vec![&sid, part]
                } else {
                    vec![&sid]
                };
                let vectors = statement
                    .query_map(params.as_slice(), |row| {
                        let bytes: Vec<u8> = row.get(0)?;
                        let mut vector: Vec<_> = bytes
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| f32::from_ne_bytes(*b) as f64)
                            .collect();
                        let norm = vector.iter().map(|n| n * n).sum::<f64>().sqrt();
                        let norm = if norm == 0. { 1. } else { norm };
                        for n in &mut vector {
                            *n /= norm;
                        }
                        Ok(vector)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(vectors)
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_db_reads_never_create_or_migrate() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        assert!(Db::open(&p).is_err());
        assert!(!p.db.exists());
        std::fs::create_dir_all(p.db.parent().unwrap()).unwrap();
        let c = rusqlite::Connection::open(&p.db).unwrap();
        c.execute_batch("create table tiny(n);insert into tiny values(7)")
            .unwrap();
        drop(c);
        let before = std::fs::read(&p.db).unwrap();
        let db = Db::open(&p).unwrap();
        assert_eq!(db.scalar("select n from tiny", &[]), 7);
        assert!(db.query("insert into tiny values(8)", &[]).is_empty());
        assert_eq!(std::fs::read(&p.db).unwrap(), before);
    }
}

#[cfg(test)]
mod malformed_tests {
    use super::*;
    #[test]
    fn test_a_malformed_database_is_refused_without_writing_it() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        std::fs::write(&paths.db, b"not a database").unwrap();
        assert!(Db::open(&paths).is_err());
        assert_eq!(std::fs::read(&paths.db).unwrap(), b"not a database");
    }
}
