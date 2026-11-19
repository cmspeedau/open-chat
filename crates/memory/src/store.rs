//! The dual-tier store: FTS5 + sqlite-vec, unified behind upsert/recall.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;
use crate::{MemoryError, ensure_vec_extension};

/// Which memory tier an entry lives in (README "Scoped Memory Tiers").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    /// `~/.openchat/memory.sqlite`: user habits and preferences.
    Global,
    /// `.openchat/memory.sqlite`: this workspace's decisions.
    Project,
    /// RAM/WAL scratch working set (persisted the same way, tagged apart).
    Session,
}

impl MemoryScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Session => "session",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project),
            "session" => Ok(Self::Session),
            other => Err(MemoryError::Sql(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(format!("unknown scope {other:?}"))),
            ))),
        }
    }
}

/// A single remembered fact. `embedding` is optional: text-only entries are
/// still sparse-recallable, and the dense tier fills in once the LLM layer
/// (a later phase) provides vectors.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEntry {
    pub scope: MemoryScope,
    pub category: String,
    pub key: String,
    pub value: String,
    pub embedding: Option<Vec<f32>>,
}

/// Where a recall hit came from, surfaced for the compositor's telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitSource {
    Sparse,
    Dense,
    Both,
}

/// One fused recall result.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallHit {
    pub entry: MemoryEntry,
    /// Reciprocal Rank Fusion score (higher is better).
    pub score: f64,
    pub source: HitSource,
}

/// Rough token estimate: cl100k averages ~4 bytes/token for prose. Budget
/// math here only needs to be stable, not exact; the compositor (Phase 4)
/// owns the real tokenizer.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Little-endian f32 blob, the wire format `vec0` KNN queries take.
#[must_use]
pub fn vector_blob(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for v in vector {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Per-connection defaults the README asks for: WAL for concurrent
/// readers, bounded busy-wait instead of `SQLITE_BUSY` spews.
fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn create_schema(conn: &Connection, dims: usize) -> Result<()> {
    conn.execute_batch(&format!(
        r#"
        CREATE TABLE IF NOT EXISTS memories (
            id         INTEGER PRIMARY KEY,
            scope      TEXT NOT NULL,
            category   TEXT NOT NULL DEFAULT 'general',
            key        TEXT NOT NULL,
            value      TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            UNIQUE(scope, key)
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            key, value, content='memories', content_rowid='id'
        );
        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, key, value) VALUES (new.id, new.key, new.value);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, key, value)
            VALUES ('delete', old.id, old.key, old.value);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, key, value)
            VALUES ('delete', old.id, old.key, old.value);
            INSERT INTO memories_fts(rowid, key, value) VALUES (new.id, new.key, new.value);
        END;
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_vec USING vec0(
            memory_id INTEGER PRIMARY KEY,
            embedding float[{dims}] distance_metric=cosine
        );
        "#
    ))?;
    Ok(())
}

/// Embedded dual-tier memory. Not internally synchronized: the daemon owns
/// one store per connection behind its own lock or actor task.
#[derive(Debug)]
pub struct MemoryStore {
    conn: Connection,
    dims: usize,
}

impl MemoryStore {
    /// Open (creating if needed) the store at `path`.
    pub fn open(path: &Path, vector_dims: usize) -> Result<Self> {
        ensure_vec_extension();
        if let Some(parent) = path.parent()
            && parent.as_os_str() != ""
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::finish_open(conn, vector_dims)
    }

    /// In-memory store for tests and session-working-tier use.
    pub fn open_in_memory(vector_dims: usize) -> Result<Self> {
        ensure_vec_extension();
        let conn = Connection::open_in_memory()?;
        Self::finish_open(conn, vector_dims)
    }

    fn finish_open(conn: Connection, vector_dims: usize) -> Result<Self> {
        let dims = vector_dims.max(1);
        configure(&conn)?;
        create_schema(&conn, dims)?;
        Ok(Self { conn, dims })
    }

    #[must_use]
    pub const fn vector_dims(&self) -> usize {
        self.dims
    }

    /// Insert or overwrite by `(scope, key)`: the contradiction-resolution
    /// rule. Returns the row id.
    pub fn store(&self, entry: &MemoryEntry) -> Result<i64> {
        if let Some(embedding) = &entry.embedding
            && embedding.len() != self.dims
        {
            return Err(MemoryError::WrongDimensions {
                got: embedding.len(),
                expected: self.dims,
            });
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO memories (scope, category, key, value, updated_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(scope, key) DO UPDATE SET
               category = excluded.category,
               value    = excluded.value,
               updated_at = excluded.updated_at",
            params![entry.scope.as_str(), entry.category, entry.key, entry.value],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM memories WHERE scope = ?1 AND key = ?2",
            params![entry.scope.as_str(), entry.key],
            |r| r.get(0),
        )?;
        match &entry.embedding {
            Some(vector) => {
                // vec0 supports neither UPSERT nor REPLACE on its PK;
                // delete-then-insert inside this tx is the documented path.
                tx.execute("DELETE FROM memories_vec WHERE memory_id = ?1", params![id])?;
                tx.execute(
                    "INSERT INTO memories_vec(memory_id, embedding) VALUES (?1, ?2)",
                    params![id, vector_blob(vector)],
                )?;
            }
            // Re-storing without an embedding drops any stale vector, so
            // dense recall never returns a value the sparse tier no longer has.
            None => {
                tx.execute("DELETE FROM memories_vec WHERE memory_id = ?1", params![id])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    /// Exact `(scope, key)` fetch.
    pub fn get(&self, scope: MemoryScope, key: &str) -> Result<Option<MemoryEntry>> {
        let row = self
            .conn
            .query_row(
                "SELECT scope, category, key, value FROM memories
                 WHERE scope = ?1 AND key = ?2",
                params![scope.as_str(), key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((scope_s, category, key, value)) = row else {
            return Ok(None);
        };
        Ok(Some(MemoryEntry {
            scope: MemoryScope::parse(&scope_s)?,
            category,
            key,
            value,
            embedding: None,
        }))
    }

    /// Forget one entry; returns true when something was removed. Keeps the
    /// dense tier in step with the sparse row delete.
    pub fn forget(&self, scope: MemoryScope, key: &str) -> Result<bool> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM memories WHERE scope = ?1 AND key = ?2",
                params![scope.as_str(), key],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(false);
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM memories_vec WHERE memory_id = ?1", params![id])?;
        tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Hybrid recall: FTS5 keywords and (when `query_embedding` is given)
    /// vec0 cosine kNN, fused by Reciprocal Rank Fusion, trimmed to a token
    /// budget (README: 500-1,000 injected per turn; the caller decides).
    pub fn recall(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        budget_tokens: usize,
    ) -> Result<Vec<RecallHit>> {
        let k = (budget_tokens.max(1) / 50).clamp(4, 24); // per-tier candidate width
        let sparse = self.sparse_ids(query, k)?;
        let dense = match query_embedding {
            Some(vector) if vector.len() == self.dims => self.dense_ids(vector, k)?,
            Some(vector) => {
                return Err(MemoryError::WrongDimensions {
                    got: vector.len(),
                    expected: self.dims,
                });
            }
            None => Vec::new(),
        };

        // RRF: score = Σ 1/(60 + rank). Constants follow the standard
        // Cormack et al. formulation; rank 1 (best) is added as 1 here.
        const RRF_K: f64 = 60.0;
        let mut fused: std::collections::BTreeMap<i64, (f64, HitSource)> =
            std::collections::BTreeMap::new();
        for (rank, id) in sparse.into_iter().enumerate() {
            let e = fused.entry(id).or_insert((0.0, HitSource::Sparse));
            e.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
        for (rank, id) in dense.into_iter().enumerate() {
            match fused.get_mut(&id) {
                Some((score, source)) => {
                    *score += 1.0 / (RRF_K + rank as f64 + 1.0);
                    *source = HitSource::Both;
                }
                None => {
                    fused.insert(id, (1.0 / (RRF_K + rank as f64 + 1.0), HitSource::Dense));
                }
            }
        }

        let mut ranked: Vec<(i64, f64, HitSource)> = fused
            .into_iter()
            .map(|(id, (score, source))| (id, score, source))
            .collect();
        // Deterministic order: score desc, id asc for ties (stable budget cut).
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });

        let mut hits = Vec::new();
        let mut spent = 0usize;
        for (id, score, source) in ranked {
            let Some(entry) = self.entry_by_id(id)? else {
                continue; // row vanished between ranking and fetch
            };
            let cost = estimate_tokens(&entry.key) + estimate_tokens(&entry.value) + 8;
            if !hits.is_empty() && spent + cost > budget_tokens {
                break;
            }
            spent += cost;
            hits.push(RecallHit {
                entry,
                score,
                source,
            });
        }
        Ok(hits)
    }

    /// Total stored entries (for startup logs / telemetry).
    pub fn count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_, i64>(0))?
            as usize)
    }

    /// WAL hygiene: truncate the log and reclaim pages. Called on idle or
    /// under memory pressure by the daemon's maintenance loop.
    pub fn compact(&self) -> Result<()> {
        self.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        self.conn.execute_batch("VACUUM; ANALYZE;")?;
        Ok(())
    }

    /// Turn free text into a safe FTS5 OR-query of bare tokens. Stripping
    /// all non-alphanumerics keeps untrusted input out of FTS syntax
    /// (quotes, `NEAR`, column filters), at the cost of advanced operators.
    fn fts_query(&self, query: &str) -> Result<String> {
        let tokens: Vec<String> = query
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if tokens.is_empty() {
            return Err(MemoryError::EmptyQuery);
        }
        Ok(tokens.join(" OR "))
    }

    fn sparse_ids(&self, query: &str, k: usize) -> Result<Vec<i64>> {
        let match_query = self.fts_query(query)?;
        let mut stmt = self.conn.prepare(
            "SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1
                          ORDER BY bm25(memories_fts) LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(params![match_query, k as i64], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    fn dense_ids(&self, query_vector: &[f32], k: usize) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT memory_id FROM memories_vec
             WHERE embedding MATCH ?1 AND k = ?2
             ORDER BY distance",
        )?;
        let ids = stmt
            .query_map(params![vector_blob(query_vector), k as i64], |r| {
                r.get::<_, i64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    fn entry_by_id(&self, id: i64) -> Result<Option<MemoryEntry>> {
        let row = self
            .conn
            .query_row(
                "SELECT scope, category, key, value FROM memories WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((scope_s, category, key, value)) = row else {
            return Ok(None);
        };
        Ok(Some(MemoryEntry {
            scope: MemoryScope::parse(&scope_s)?,
            category,
            key,
            value,
            embedding: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MemoryScope::*;

    fn entry(key: &str, value: &str, embedding: Option<&[f32]>) -> MemoryEntry {
        MemoryEntry {
            scope: Project,
            category: "conventions".into(),
            key: key.into(),
            value: value.into(),
            embedding: embedding.map(<[f32]>::to_vec),
        }
    }

    fn store() -> MemoryStore {
        MemoryStore::open_in_memory(4).unwrap()
    }

    #[test]
    fn sparse_recall_finds_keyword_entry() {
        let s = store();
        s.store(&entry("editor", "user prefers tabs, never spaces", None))
            .unwrap();
        s.store(&entry("build", "cargo nextest is the test runner", None))
            .unwrap();
        let hits = s.recall("tabs indentation", None, 500).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.key, "editor");
        assert_eq!(hits[0].source, HitSource::Sparse);
    }

    #[test]
    fn upsert_resolves_contradictions_in_both_tiers() {
        let s = store();
        s.store(&entry(
            "state",
            "use redux for state management",
            Some(&[1., 0., 0., 0.]),
        ))
        .unwrap();
        s.store(&entry(
            "state",
            "use zustand for state management",
            Some(&[0., 1., 0., 0.]),
        ))
        .unwrap();
        assert_eq!(s.count().unwrap(), 1, "must not duplicate");
        let got = s.get(Project, "state").unwrap().unwrap();
        assert!(got.value.contains("zustand"));
        // Old value is gone from the sparse tier (trigger-synced FTS).
        assert!(s.recall("redux", None, 500).unwrap().is_empty());
        // And the stale vector no longer wins dense kNN.
        let hits = s.recall("state", Some(&[1., 0., 0., 0.]), 500).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].entry.value.contains("zustand"));
    }

    #[test]
    fn dense_recall_ranks_nearest_embedding_first() {
        let s = store();
        s.store(&entry(
            "rust",
            "rustacean conventions",
            Some(&[1., 0., 0., 0.]),
        ))
        .unwrap();
        s.store(&entry("python", "pythonic idioms", Some(&[0., 1., 0., 0.])))
            .unwrap();
        s.store(&entry(
            "mixed",
            "some other note",
            Some(&[0.9, 0.1, 0., 0.]),
        ))
        .unwrap();
        let err = s.recall("", Some(&[1., 0., 0., 0.]), 500).unwrap_err();
        // Empty text query is rejected even with an embedding: the FTS tier
        // needs tokens; callers pass keywords plus vector, not vector alone.
        assert!(matches!(err, MemoryError::EmptyQuery));

        let hits = s
            .recall("rust python", Some(&[1., 0., 0., 0.]), 500)
            .unwrap();
        assert_eq!(hits[0].entry.key, "rust");
        assert_eq!(hits[0].source, HitSource::Both, "agreement across tiers");
    }

    #[test]
    fn hybrid_rrf_prefers_agreement() {
        let s = store();
        // agreed: matched by both keyword and vector.
        s.store(&entry(
            "agreed",
            "kubernetes helm charts",
            Some(&[1., 0., 0., 0.]),
        ))
        .unwrap();
        // keyword-only match.
        s.store(&entry(
            "wordy",
            "kubernetes operator notes",
            Some(&[0., 0., 1., 0.]),
        ))
        .unwrap();
        // vector-only match.
        s.store(&entry(
            "veconly",
            "unrelated prose",
            Some(&[0.99, 0.01, 0., 0.]),
        ))
        .unwrap();
        let hits = s
            .recall("kubernetes", Some(&[1., 0., 0., 0.]), 500)
            .unwrap();
        assert_eq!(hits[0].entry.key, "agreed");
        assert!(hits[0].score > hits[1].score);
        assert!(hits.iter().any(|h| h.entry.key == "veconly"));
    }

    #[test]
    fn budget_trims_results() {
        let s = store();
        for i in 0..10 {
            s.store(&entry(
                &format!("k{i}"),
                &"al ".repeat(133), // one repeating token, ~100 estimated tokens
                None,
            ))
            .unwrap();
        }
        let hits = s.recall("al", None, 120).unwrap();
        assert_eq!(hits.len(), 1, "one ~109-token hit fits, second must not");
    }

    #[test]
    fn forget_keeps_tiers_in_step() {
        let s = store();
        s.store(&entry("gone", "temporary note", Some(&[0., 0., 0., 1.])))
            .unwrap();
        assert!(s.forget(Project, "gone").unwrap());
        assert!(!s.forget(Project, "gone").unwrap());
        let hits = s.recall("temporary", None, 500).unwrap();
        assert!(hits.is_empty(), "sparse tier cleaned");
    }

    #[test]
    fn wrong_dims_rejected() {
        let s = store();
        let e = entry("bad", "value", Some(&[1., 2.]));
        assert!(matches!(
            s.store(&e),
            Err(MemoryError::WrongDimensions {
                got: 2,
                expected: 4
            })
        ));
    }

    #[test]
    fn scopes_are_isolated() {
        let s = store();
        s.store(&MemoryEntry {
            scope: Global,
            ..entry("pref", "global pref", None)
        })
        .unwrap();
        s.store(&MemoryEntry {
            scope: Project,
            ..entry("pref", "project pref", None)
        })
        .unwrap();
        assert_eq!(s.count().unwrap(), 2);
        assert!(
            s.get(Global, "pref")
                .unwrap()
                .unwrap()
                .value
                .contains("global")
        );
        assert!(
            s.get(Project, "pref")
                .unwrap()
                .unwrap()
                .value
                .contains("project")
        );
    }

    #[test]
    fn fts_syntax_cannot_escape_user_query() {
        let s = store();
        s.store(&entry("safe", "plain prose about alpha", None))
            .unwrap();
        // Quotes/NEAR/column-filter attempts must not error or match-all.
        for probe in [
            "\"alpha\" NEAR beta",
            "alpha AND (NOT value:beta)",
            "alpha*",
        ] {
            let hits = s.recall(probe, None, 500).unwrap();
            assert!(hits.len() <= 1, "probe {probe:?} over-matched");
        }
        // Punctuation-only queries carry no tokens.
        assert!(matches!(
            s.recall("*** &&&", None, 500),
            Err(MemoryError::EmptyQuery)
        ));
    }

    #[test]
    fn compact_runs_clean() {
        let s = store();
        s.store(&entry("a", "note one", None)).unwrap();
        s.compact().unwrap();
        assert_eq!(s.count().unwrap(), 1);
    }
}
