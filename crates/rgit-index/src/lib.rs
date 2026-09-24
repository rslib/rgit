//! Semantic code search: chunk a repo's text files, embed the chunks with a
//! local static-embedding model (model2vec), and index the L2-normalized vectors
//! in an HNSW graph (instant-distance). The per-chunk records persist to disk so
//! a rebuild reuses unchanged files (keyed by git blob OID) and only re-embeds
//! what changed. Queries embed the same way and walk the graph by cosine.

use std::path::{Path, PathBuf};

use instant_distance::{Builder, HnswMap, Search};
use serde::{Deserialize, Serialize};

mod dynamic;

#[derive(thiserror::Error, Debug)]
pub enum IndexError {
    #[error("embedding: {0}")]
    Embed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("index format: {0}")]
    Codec(String),
}

const CHUNK_LINES: usize = 40;
const CHUNK_OVERLAP: usize = 10;
const MAX_FILE_BYTES: u64 = 512 * 1024;
/// A model2vec static-embedding model: a token->vector table plus mean pooling.
/// No transformer inference, so it is fast and light on CPU (unlike an ONNX
/// model that spins one arena per core). Downloaded once to the HF cache.
const MODEL: &str = "minishlab/potion-base-8M";

/// An embedding vector as an HNSW point. Vectors are L2-normalized, so cosine
/// distance is `1 - dot`.
#[derive(Clone, Serialize, Deserialize)]
pub struct Point(Vec<f32>);

impl instant_distance::Point for Point {
    fn distance(&self, other: &Self) -> f32 {
        1.0 - self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum::<f32>()
    }
}

/// One indexed span of a file. `preview` is the first few lines, kept for display
/// without re-reading the file.
#[derive(Serialize, Deserialize, Clone)]
pub struct Chunk {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub preview: String,
}

/// A chunk plus its embedding and the git blob OID of the file it came from. The
/// OID lets a rebuild reuse a file's records verbatim when its content is
/// unchanged, so only edited files are re-embedded.
#[derive(Serialize, Deserialize, Clone)]
struct Record {
    oid: String,
    chunk: Chunk,
    vec: Vec<f32>,
}

/// What a build reused versus recomputed.
pub struct BuildStats {
    pub total: usize,
    pub reused: usize,
    pub embedded: usize,
}

/// A repo's semantic index: the persisted per-chunk records (source of truth for
/// incremental rebuilds) plus an in-memory HNSW graph over their vectors, built
/// on load. The graph maps a point to its record index.
pub struct Index {
    records: Vec<Record>,
    hnsw: HnswMap<Point, u32>,
}

impl Index {
    fn from_records(records: Vec<Record>) -> Self {
        let points: Vec<Point> = records.iter().map(|r| Point(r.vec.clone())).collect();
        let ids: Vec<u32> = (0..records.len() as u32).collect();
        let hnsw = Builder::default().build(points, ids);
        Index { records, hnsw }
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// One ranked search result.
pub struct SearchHit {
    pub score: f32,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub preview: String,
}

/// A loaded embedding model. Constructing it downloads the model to the HF cache
/// on first use, so it is created once and reused for a whole build or query.
pub struct Embedder {
    model: model2vec_rs::model::StaticModel,
}

impl Embedder {
    pub fn new() -> Result<Self, IndexError> {
        // normalize=true so cosine similarity is a plain dot product.
        let model =
            model2vec_rs::model::StaticModel::from_pretrained(MODEL, None, Some(true), None)
                .map_err(|e| IndexError::Embed(e.to_string()))?;
        Ok(Self { model })
    }

    /// Embed a batch of texts. Static embeddings are just table lookups plus mean
    /// pooling, so this is fast and allocates almost nothing.
    pub fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, IndexError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = self.model.encode(&texts);
        for v in &mut out {
            normalize(v);
        }
        Ok(out)
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v {
            *x /= norm;
        }
    }
}

fn is_probably_text(bytes: &[u8]) -> bool {
    !bytes.iter().take(8000).any(|&b| b == 0)
}

/// A chunk of a file to embed: line range, the text body, and an optional symbol
/// name (from tree-sitter) that is prepended to the embedding input.
struct FileChunk {
    start: usize,
    end: usize,
    body: String,
    name: String,
}

/// Resolve a file to a bundled tree-sitter language and its tags query, by
/// extension. None for extensions we do not bundle (they use line windows; the
/// dynamic-grammar path fills these in later).
fn language_for(path: &str) -> Option<(tree_sitter::Language, String)> {
    use tree_sitter::Language;
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let pair = match ext.as_str() {
        "rs" => (
            Language::new(tree_sitter_rust::LANGUAGE),
            tree_sitter_rust::TAGS_QUERY,
        ),
        "py" | "pyi" => (
            Language::new(tree_sitter_python::LANGUAGE),
            tree_sitter_python::TAGS_QUERY,
        ),
        "js" | "mjs" | "cjs" | "jsx" => (
            Language::new(tree_sitter_javascript::LANGUAGE),
            tree_sitter_javascript::TAGS_QUERY,
        ),
        "ts" => (
            Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            tree_sitter_typescript::TAGS_QUERY,
        ),
        "tsx" => (
            Language::new(tree_sitter_typescript::LANGUAGE_TSX),
            tree_sitter_typescript::TAGS_QUERY,
        ),
        "go" => (
            Language::new(tree_sitter_go::LANGUAGE),
            tree_sitter_go::TAGS_QUERY,
        ),
        "c" | "h" => (
            Language::new(tree_sitter_c::LANGUAGE),
            tree_sitter_c::TAGS_QUERY,
        ),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => (
            Language::new(tree_sitter_cpp::LANGUAGE),
            tree_sitter_cpp::TAGS_QUERY,
        ),
        "java" => (
            Language::new(tree_sitter_java::LANGUAGE),
            tree_sitter_java::TAGS_QUERY,
        ),
        // Not bundled: try a dynamically-built grammar (nvim-style), else fall
        // back to line windows.
        other => return dynamic::language_for(other),
    };
    Some((pair.0, pair.1.to_owned()))
}

/// Extract each top-level definition (function, class, method, module, ...) as a
/// chunk, using the grammar's tags query. None when the language is not bundled
/// or nothing parses, so the caller falls back to line windows.
fn symbol_chunks(path: &str, text: &str) -> Option<Vec<FileChunk>> {
    use streaming_iterator::StreamingIterator;
    let (lang, tags_src) = language_for(path)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(text, None)?;
    if tags_src.is_empty() {
        return None;
    }
    let query = tree_sitter::Query::new(&lang, &tags_src).ok()?;
    let names = query.capture_names();
    let src = text.as_bytes();

    let mut out: Vec<FileChunk> = Vec::new();
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), src);
    while let Some(m) = it.next() {
        let mut def = None;
        let mut name = String::new();
        for c in m.captures {
            let cap = names[c.index as usize];
            if cap.starts_with("definition") {
                def = Some(c.node);
            } else if cap == "name" {
                name = c.node.utf8_text(src).unwrap_or("").to_owned();
            }
        }
        if let Some(node) = def {
            let start = node.start_position().row + 1;
            let end = node.end_position().row + 1;
            // The tags query can match one node under several capture names; keep
            // each line range once.
            if !seen.insert((start, end)) {
                continue;
            }
            let body = node.utf8_text(src).unwrap_or("").to_owned();
            // Very large definitions dilute a mean-pooled embedding; window them.
            if end - start + 1 > CHUNK_LINES * 2 {
                for (s, e, b) in chunk_text(&body) {
                    out.push(FileChunk {
                        start: start + s - 1,
                        end: start + e - 1,
                        body: b,
                        name: name.clone(),
                    });
                }
            } else if body.trim().len() >= 3 {
                out.push(FileChunk {
                    start,
                    end,
                    body,
                    name: name.clone(),
                });
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// The chunks to embed for a file: tree-sitter definitions when the language is
/// supported, else overlapping line windows.
fn file_chunks(path: &str, text: &str) -> Vec<FileChunk> {
    if let Some(chunks) = symbol_chunks(path, text) {
        return chunks;
    }
    chunk_text(text)
        .into_iter()
        .map(|(start, end, body)| FileChunk {
            start,
            end,
            body,
            name: String::new(),
        })
        .collect()
}

/// Split a file into overlapping line windows, dropping windows that are only
/// whitespace.
fn chunk_text(text: &str) -> Vec<(usize, usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let step = CHUNK_LINES.saturating_sub(CHUNK_OVERLAP).max(1);
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let end = (i + CHUNK_LINES).min(lines.len());
        let body = lines[i..end].join("\n");
        if body.trim().len() >= 3 {
            out.push((i + 1, end, body));
        }
        if end == lines.len() {
            break;
        }
        i += step;
    }
    out
}

/// The git blob OID of some bytes: `sha1("blob " + len + "\0" + content)`. This
/// matches `git hash-object` without needing libgit2, so a file's OID is stable
/// across commits/branches and identical files share an OID.
fn blob_oid(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Build an index over a working directory, embedding with a freshly loaded
/// model and reusing a previous index's unchanged files.
pub fn build(workdir: &Path, previous: Option<&Index>) -> Result<(Index, BuildStats), IndexError> {
    build_with(workdir, &Embedder::new()?, previous)
}

/// Build an index using an existing [`Embedder`]. When `previous` is given, a
/// file whose blob OID is unchanged reuses its cached records (chunks + vectors)
/// and is not re-embedded; only new or edited files are embedded.
pub fn build_with(
    workdir: &Path,
    embedder: &Embedder,
    previous: Option<&Index>,
) -> Result<(Index, BuildStats), IndexError> {
    // Previous records grouped by path, with the OID they were built from.
    let mut prev_by_path: std::collections::HashMap<&str, (&str, Vec<&Record>)> =
        std::collections::HashMap::new();
    if let Some(p) = previous {
        for r in &p.records {
            prev_by_path
                .entry(r.chunk.path.as_str())
                .or_insert((r.oid.as_str(), Vec::new()))
                .1
                .push(r);
        }
    }

    let mut records: Vec<Record> = Vec::new();
    let mut reused = 0usize;
    // Chunks that still need embedding: (oid, chunk, embed_input).
    let mut pending: Vec<(String, Chunk, String)> = Vec::new();

    for entry in ignore::WalkBuilder::new(workdir).build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = path.metadata() else { continue };
        if meta.len() == 0 || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if !is_probably_text(&bytes) {
            continue;
        }
        let oid = blob_oid(&bytes);
        let rel = path
            .strip_prefix(workdir)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();

        // Unchanged file: reuse its records verbatim.
        if let Some((prev_oid, recs)) = prev_by_path.get(rel.as_str()) {
            if *prev_oid == oid {
                reused += recs.len();
                records.extend(recs.iter().map(|r| (*r).clone()));
                continue;
            }
        }

        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        for fc in file_chunks(&rel, &text) {
            let preview = fc.body.lines().take(3).collect::<Vec<_>>().join("\n");
            let chunk = Chunk {
                path: rel.clone(),
                start_line: fc.start,
                end_line: fc.end,
                preview,
            };
            // Prefix the path (and symbol name, when known) so they participate
            // in the embedding.
            let input = if fc.name.is_empty() {
                format!("{rel}\n{}", fc.body)
            } else {
                format!("{rel} {}\n{}", fc.name, fc.body)
            };
            pending.push((oid.clone(), chunk, input));
        }
    }

    let embedded = pending.len();
    let texts: Vec<String> = pending.iter().map(|(_, _, t)| t.clone()).collect();
    for ((oid, chunk, _), vec) in pending.into_iter().zip(embedder.embed(texts)?) {
        records.push(Record { oid, chunk, vec });
    }

    let stats = BuildStats {
        total: records.len(),
        reused,
        embedded,
    };
    Ok((Index::from_records(records), stats))
}

/// Rank the index's chunks against `query` and return the top `k` hits.
pub fn search(
    index: &Index,
    embedder: &Embedder,
    query: &str,
    k: usize,
) -> Result<Vec<SearchHit>, IndexError> {
    if index.is_empty() || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let q = embedder
        .embed(vec![query.to_owned()])?
        .pop()
        .map(Point)
        .ok_or_else(|| IndexError::Embed("empty query embedding".into()))?;

    let mut search = Search::default();
    Ok(index
        .hnsw
        .search(&q, &mut search)
        .take(k)
        .map(|item| {
            let c = &index.records[*item.value as usize].chunk;
            SearchHit {
                score: 1.0 - item.distance,
                path: c.path.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                preview: c.preview.clone(),
            }
        })
        .collect())
}

/// Like [`search`], but multiply each hit's semantic score by
/// `1 + alpha * boost[path]` before ranking, so a per-path history weight (churn
/// and recency) reorders results without displacing genuinely relevant ones. A
/// larger candidate pool is scored so boosting can promote a hit past the plain
/// semantic top-`k`.
pub fn search_boosted(
    index: &Index,
    embedder: &Embedder,
    query: &str,
    k: usize,
    boost: &std::collections::HashMap<String, f32>,
    alpha: f32,
) -> Result<Vec<SearchHit>, IndexError> {
    if index.is_empty() || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let q = embedder
        .embed(vec![query.to_owned()])?
        .pop()
        .map(Point)
        .ok_or_else(|| IndexError::Embed("empty query embedding".into()))?;

    let pool = (k * 5).max(50);
    let mut search = Search::default();
    let mut hits: Vec<SearchHit> = index
        .hnsw
        .search(&q, &mut search)
        .take(pool)
        .map(|item| {
            let c = &index.records[*item.value as usize].chunk;
            let semantic = 1.0 - item.distance;
            let weight = boost.get(&c.path).copied().unwrap_or(0.0);
            SearchHit {
                score: semantic * (1.0 + alpha * weight),
                path: c.path.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                preview: c.preview.clone(),
            }
        })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(k);
    Ok(hits)
}

/// Where a repo's index is stored: under `.git/rgit/` for a normal repo, or the
/// working dir itself when `.git` is not a directory (e.g. a linked worktree).
pub fn index_path(workdir: &Path) -> PathBuf {
    let git = workdir.join(".git");
    let base = if git.is_dir() {
        git
    } else {
        workdir.to_path_buf()
    };
    base.join("rgit").join("semantic.bin")
}

pub fn save(index: &Index, path: &Path) -> Result<(), IndexError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Persist only the records; the HNSW graph is rebuilt from them on load.
    let bytes = bincode::serialize(&index.records).map_err(|e| IndexError::Codec(e.to_string()))?;
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn load(path: &Path) -> Option<Index> {
    let records: Vec<Record> = bincode::deserialize(&std::fs::read(path).ok()?).ok()?;
    Some(Index::from_records(records))
}
