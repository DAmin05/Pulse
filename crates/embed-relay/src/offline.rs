//! `embed-relay embed-fixture IN OUT`: embed a raw article fixture (.pulsefx)
//! into an embedded fixture (.pulseem) without Kafka, preserving order.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use pulse_core::fixture::{self, FixtureWriter};
use pulse_core::proto::v1::{Article, EmbeddedArticle};

use crate::embedder::Embedder;
use crate::relay::embedding_text;

const CHUNK: usize = 512;

pub async fn embed_fixture(embedder: &Embedder, input: &Path, output: &Path) -> Result<()> {
    let articles: Vec<Article> =
        fixture::read_all(input).with_context(|| format!("reading {}", input.display()))?;
    let mut writer = FixtureWriter::<EmbeddedArticle>::create(output)?;
    let started = Instant::now();
    for (i, chunk) in articles.chunks(CHUNK).enumerate() {
        let embedded = embedder
            .embed_passages(chunk.iter().map(embedding_text).collect())
            .await?;
        for (article, vector) in chunk.iter().zip(embedded.vectors) {
            writer.write(&EmbeddedArticle {
                article: Some(article.clone()),
                vector,
                model_version: embedded.model_version.clone(),
            })?;
        }
        let done = (i * CHUNK + chunk.len()) as f64;
        tracing::info!(
            "{done}/{} ({:.0} articles/s)",
            articles.len(),
            done / started.elapsed().as_secs_f64()
        );
    }
    let written = writer.finish()?;
    tracing::info!(written, out = %output.display(), "embedded fixture written");
    Ok(())
}
