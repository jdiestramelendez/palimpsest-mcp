//! Retrieval eval harness (ticket 005): seeds an in-memory DB from
//! `evals/dataset.json` and scores search quality as hit@k and MRR,
//! overall and per query category.
//!
//! - `--provider mock` (default): deterministic MockEmbedder; fixture
//!   `vector_groups` pin same-axis vectors for semantic cases. CI-safe.
//! - `--provider ollama`: real embeddings via the config's Ollama model
//!   (pins ignored); local-only.
//! - `--check`: exit non-zero when overall metrics fall below the
//!   committed floors (used by CI; floors documented in evals/RESULTS.md).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use serde::Deserialize;

use palimpsest_mcp::config::Config;
use palimpsest_mcp::core::retrieval::{self, RecencyBoost, SearchParams};
use palimpsest_mcp::db::{queries, Db};
use palimpsest_mcp::providers::mock::MockEmbedder;
use palimpsest_mcp::providers::ollama::OllamaEmbedder;
use palimpsest_mcp::providers::Embedder;

/// Metric floors enforced by `--check` (baseline minus a safety margin;
/// see evals/RESULTS.md for the committed baseline).
const CHECK_MIN_HIT8: f64 = 0.90;
const CHECK_MIN_MRR: f64 = 0.70;

/// CLI arguments.
#[derive(Debug, Parser)]
#[command(name = "eval", about = "palimpsest-mcp retrieval eval harness")]
struct Cli {
    /// Dataset fixture path.
    #[arg(long, default_value = "evals/dataset.json")]
    dataset: PathBuf,
    /// Embedding provider: deterministic mock, or the config's Ollama.
    #[arg(long, value_parser = ["mock", "ollama"], default_value = "mock")]
    provider: String,
    /// Enforce the committed metric floors (CI mode).
    #[arg(long)]
    check: bool,
}

#[derive(Debug, Deserialize)]
struct Dataset {
    dims: usize,
    #[serde(default)]
    vector_groups: Vec<VectorGroup>,
    memories: Vec<FixtureMemory>,
    queries: Vec<FixtureQuery>,
}

#[derive(Debug, Deserialize)]
struct VectorGroup {
    axis: usize,
    texts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FixtureMemory {
    id: String,
    /// Project slug; `null` = global memory.
    project: Option<String>,
    content: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default = "default_importance")]
    importance: i64,
    #[serde(default)]
    entities: Vec<FixtureEntity>,
    /// Fixture id of the memory this one supersedes.
    #[serde(default)]
    supersedes: Option<String>,
    /// Shift valid_at this many days into the past (as_of scenarios).
    #[serde(default)]
    backdate_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct FixtureEntity {
    name: String,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FixtureQuery {
    id: String,
    query: String,
    #[serde(default)]
    project: Option<String>,
    expected: Vec<String>,
    category: String,
    #[serde(default)]
    as_of_days_ago: Option<i64>,
}

fn default_kind() -> String {
    "fact".into()
}
fn default_importance() -> i64 {
    3
}

/// Per-bucket accumulator: reciprocal ranks of the first relevant hit.
#[derive(Debug, Default)]
struct Bucket {
    reciprocal_ranks: Vec<f64>,
}

impl Bucket {
    fn record(&mut self, first_relevant_rank: Option<usize>) {
        self.reciprocal_ranks
            .push(first_relevant_rank.map_or(0.0, |r| 1.0 / r as f64));
    }
    fn hit_at(&self, k: usize) -> f64 {
        let hits = self
            .reciprocal_ranks
            .iter()
            .filter(|rr| **rr > 0.0 && (1.0 / **rr).round() as usize <= k)
            .count();
        hits as f64 / self.reciprocal_ranks.len().max(1) as f64
    }
    fn mrr(&self) -> f64 {
        self.reciprocal_ranks.iter().sum::<f64>() / self.reciprocal_ranks.len().max(1) as f64
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let raw = std::fs::read_to_string(&cli.dataset)?;
    let dataset: Dataset = serde_json::from_str(&raw)?;

    let (embedder, dims): (Arc<dyn Embedder>, usize) = match cli.provider.as_str() {
        "ollama" => {
            let config = Config::load()?;
            let e = OllamaEmbedder::new(
                config.embedder.base_url.clone(),
                config.embedder.model.clone(),
                config.embedder.dims,
            );
            (Arc::new(e), config.embedder.dims)
        }
        _ => {
            let mock = MockEmbedder::new(dataset.dims);
            for g in &dataset.vector_groups {
                let mut v = vec![0.0f32; dataset.dims];
                v[g.axis % dataset.dims] = 1.0;
                for text in &g.texts {
                    mock.pin(text, v.clone());
                }
            }
            (Arc::new(mock), dataset.dims)
        }
    };

    let mut db = Db::open_in_memory(dims)?;

    // Seed: embed in batch, insert through the query layer, then apply
    // fixture-only surgery (backdating) for the as_of scenarios.
    let contents: Vec<String> = dataset.memories.iter().map(|m| m.content.clone()).collect();
    let vectors = embedder.embed(&contents).await?;
    let mut id_map: HashMap<String, i64> = HashMap::new();
    for (m, vector) in dataset.memories.iter().zip(vectors) {
        if let Some(p) = &m.project {
            queries::ensure_project(db.conn(), p)?;
        }
        let graph = (!m.entities.is_empty()).then(|| queries::GraphPayload {
            entities: m
                .entities
                .iter()
                .map(|e| queries::EntityRef {
                    name: e.name.clone(),
                    kind: e.kind.clone().unwrap_or_else(|| "other".into()),
                })
                .collect(),
            triples: vec![],
        });
        let supersedes = m
            .supersedes
            .as_ref()
            .map(|fid| *id_map.get(fid).expect("fixture supersedes forward ref"));
        let row_id = queries::insert_memory(
            db.conn_mut(),
            &queries::NewMemory {
                project_id: m.project.clone(),
                content: m.content.clone(),
                kind: m.kind.clone(),
                importance: m.importance,
                source_session_id: None,
            },
            &vector,
            supersedes,
            graph.as_ref(),
        )?;
        if let Some(days) = m.backdate_days {
            db.conn().execute(
                "UPDATE memories
                 SET valid_at = strftime('%Y-%m-%dT%H:%M:%fZ','now', ?1 || ' days'),
                     created_at = valid_at
                 WHERE id = ?2",
                rusqlite::params![format!("-{days}"), row_id],
            )?;
        }
        id_map.insert(m.id.clone(), row_id);
    }

    // Run the queries.
    let mut overall = Bucket::default();
    let mut by_category: HashMap<String, Bucket> = HashMap::new();
    let mut misses: Vec<String> = Vec::new();
    for q in &dataset.queries {
        let embedding = embedder
            .embed(std::slice::from_ref(&q.query))
            .await?
            .remove(0);
        let as_of = q.as_of_days_ago.map(|days| {
            (chrono::Utc::now() - chrono::Duration::days(days))
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string()
        });
        let params = SearchParams {
            project_id: q.project.clone(),
            kinds: None,
            limit: 8,
            recency: as_of.is_none().then_some(RecencyBoost {
                weight: 0.25,
                half_life_days: 30.0,
            }),
            as_of,
        };
        let hits = retrieval::search(db.conn(), &q.query, &embedding, &params)?;
        let expected: Vec<i64> = q.expected.iter().map(|fid| id_map[fid]).collect();
        let first_rank = hits
            .iter()
            .position(|h| expected.contains(&h.memory.id))
            .map(|p| p + 1);
        if first_rank.is_none() {
            misses.push(format!("  MISS {} [{}]: \"{}\"", q.id, q.category, q.query));
        }
        overall.record(first_rank);
        by_category.entry(q.category.clone()).or_default().record(first_rank);
    }

    // Report.
    println!("dataset: {} memories, {} queries | provider: {}", dataset.memories.len(), dataset.queries.len(), cli.provider);
    println!("{:<12} {:>3} {:>7} {:>7} {:>7} {:>7}", "category", "n", "hit@1", "hit@3", "hit@8", "MRR");
    let mut cats: Vec<_> = by_category.iter().collect();
    cats.sort_by_key(|(k, _)| (*k).clone());
    for (cat, b) in cats {
        println!("{:<12} {:>3} {:>7.3} {:>7.3} {:>7.3} {:>7.3}", cat, b.reciprocal_ranks.len(), b.hit_at(1), b.hit_at(3), b.hit_at(8), b.mrr());
    }
    println!("{:<12} {:>3} {:>7.3} {:>7.3} {:>7.3} {:>7.3}", "OVERALL", overall.reciprocal_ranks.len(), overall.hit_at(1), overall.hit_at(3), overall.hit_at(8), overall.mrr());
    if !misses.is_empty() {
        println!("misses:");
        for m in &misses {
            println!("{m}");
        }
    }

    if cli.check && (overall.hit_at(8) < CHECK_MIN_HIT8 || overall.mrr() < CHECK_MIN_MRR) {
        eprintln!(
            "CHECK FAILED: hit@8 {:.3} (floor {CHECK_MIN_HIT8}) / MRR {:.3} (floor {CHECK_MIN_MRR})",
            overall.hit_at(8),
            overall.mrr()
        );
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Bucket;

    #[test]
    fn bucket_metrics() {
        let mut b = Bucket::default();
        b.record(Some(1)); // rr 1.0
        b.record(Some(4)); // rr 0.25 -> outside top-3, inside top-8
        b.record(None); // miss
        assert!((b.mrr() - (1.0 + 0.25) / 3.0).abs() < 1e-9);
        assert!((b.hit_at(1) - 1.0 / 3.0).abs() < 1e-9);
        assert!((b.hit_at(3) - 1.0 / 3.0).abs() < 1e-9);
        assert!((b.hit_at(8) - 2.0 / 3.0).abs() < 1e-9);
    }
}
