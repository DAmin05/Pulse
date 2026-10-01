//! Golden fixture files: a recorded, ordered stream of articles.
//!
//! Format: the 8-byte magic `PULSEFX1`, then length-delimited protobuf
//! `pulse.v1.Article` messages. Writers emit articles in the order replay
//! should see them, normally `(fetched_at_ms, id)`.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use prost::Message;

use crate::proto::v1::Article;

const MAGIC: &[u8; 8] = b"PULSEFX1";

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("not a Pulse fixture (bad magic)")]
    BadMagic,
    #[error("corrupt record {index}: {source}")]
    Decode {
        index: usize,
        source: prost::DecodeError,
    },
}

pub struct FixtureWriter<W: Write> {
    out: W,
    count: usize,
}

impl FixtureWriter<BufWriter<File>> {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::new(BufWriter::new(File::create(path)?))
    }
}

impl<W: Write> FixtureWriter<W> {
    pub fn new(mut out: W) -> Result<Self, FixtureError> {
        out.write_all(MAGIC)?;
        Ok(Self { out, count: 0 })
    }

    pub fn write(&mut self, article: &Article) -> Result<(), FixtureError> {
        self.out
            .write_all(&article.encode_length_delimited_to_vec())?;
        self.count += 1;
        Ok(())
    }

    /// Flushes and returns the number of articles written.
    pub fn finish(mut self) -> Result<usize, FixtureError> {
        self.out.flush()?;
        Ok(self.count)
    }
}

pub fn read_all(path: impl AsRef<Path>) -> Result<Vec<Article>, FixtureError> {
    let mut bytes = Vec::new();
    BufReader::new(File::open(path)?).read_to_end(&mut bytes)?;
    decode(&bytes)
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Article>, FixtureError> {
    let mut buf = bytes.strip_prefix(MAGIC).ok_or(FixtureError::BadMagic)?;
    let mut articles = Vec::new();
    while !buf.is_empty() {
        let article =
            Article::decode_length_delimited(&mut buf).map_err(|source| FixtureError::Decode {
                index: articles.len(),
                source,
            })?;
        articles.push(article);
    }
    Ok(articles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let articles: Vec<Article> = (0..3)
            .map(|i| Article {
                id: format!("id{i}"),
                title: format!("título {i}"),
                published_at_ms: 1_700_000_000_000 + i,
                ..Default::default()
            })
            .collect();

        let mut w = FixtureWriter::new(Vec::new()).unwrap();
        for a in &articles {
            w.write(a).unwrap();
        }
        let bytes = w.out.clone();
        assert_eq!(w.finish().unwrap(), 3);
        assert_eq!(decode(&bytes).unwrap(), articles);
    }

    #[test]
    fn rejects_foreign_files() {
        assert!(matches!(decode(b"hello"), Err(FixtureError::BadMagic)));
    }
}
