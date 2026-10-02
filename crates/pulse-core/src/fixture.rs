//! Golden fixture files: a recorded, ordered stream of protobuf records.
//!
//! Format: an 8-byte magic identifying the record type, then length-delimited
//! protobuf messages. Writers emit records in the order replay should see them:
//! raw articles by `(fetched_at_ms, id)`, embedded articles in log order.
//!
//! | magic      | record                     | extension  |
//! |------------|----------------------------|------------|
//! | `PULSEFX1` | `pulse.v1.Article`         | `.pulsefx` |
//! | `PULSEEM1` | `pulse.v1.EmbeddedArticle` | `.pulseem` |

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::marker::PhantomData;
use std::path::Path;

use prost::Message;

use crate::proto::v1::{Article, EmbeddedArticle};

/// A message type that can be stored in a fixture file.
pub trait FixtureRecord: Message + Default {
    const MAGIC: &'static [u8; 8];
}

impl FixtureRecord for Article {
    const MAGIC: &'static [u8; 8] = b"PULSEFX1";
}

impl FixtureRecord for EmbeddedArticle {
    const MAGIC: &'static [u8; 8] = b"PULSEEM1";
}

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("not a fixture of the expected record type (bad magic)")]
    BadMagic,
    #[error("corrupt record {index}: {source}")]
    Decode {
        index: usize,
        source: prost::DecodeError,
    },
}

/// The 8-byte magic at the start of a fixture file, if it has one.
pub fn magic(path: impl AsRef<Path>) -> Result<[u8; 8], FixtureError> {
    let mut magic = [0u8; 8];
    File::open(path)?.read_exact(&mut magic)?;
    Ok(magic)
}

pub struct FixtureWriter<M: FixtureRecord, W: Write = BufWriter<File>> {
    out: W,
    count: usize,
    _record: PhantomData<M>,
}

impl<M: FixtureRecord> FixtureWriter<M> {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::new(BufWriter::new(File::create(path)?))
    }
}

impl<M: FixtureRecord, W: Write> FixtureWriter<M, W> {
    pub fn new(mut out: W) -> Result<Self, FixtureError> {
        out.write_all(M::MAGIC)?;
        Ok(Self {
            out,
            count: 0,
            _record: PhantomData,
        })
    }

    pub fn write(&mut self, record: &M) -> Result<(), FixtureError> {
        self.out
            .write_all(&record.encode_length_delimited_to_vec())?;
        self.count += 1;
        Ok(())
    }

    /// Flushes and returns the number of records written.
    pub fn finish(mut self) -> Result<usize, FixtureError> {
        self.out.flush()?;
        Ok(self.count)
    }
}

pub fn read_all<M: FixtureRecord>(path: impl AsRef<Path>) -> Result<Vec<M>, FixtureError> {
    let mut bytes = Vec::new();
    BufReader::new(File::open(path)?).read_to_end(&mut bytes)?;
    decode(&bytes)
}

pub fn decode<M: FixtureRecord>(bytes: &[u8]) -> Result<Vec<M>, FixtureError> {
    let mut buf = bytes.strip_prefix(M::MAGIC).ok_or(FixtureError::BadMagic)?;
    let mut records = Vec::new();
    while !buf.is_empty() {
        let record =
            M::decode_length_delimited(&mut buf).map_err(|source| FixtureError::Decode {
                index: records.len(),
                source,
            })?;
        records.push(record);
    }
    Ok(records)
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

        let mut w = FixtureWriter::<Article, _>::new(Vec::new()).unwrap();
        for a in &articles {
            w.write(a).unwrap();
        }
        let bytes = w.out.clone();
        assert_eq!(w.finish().unwrap(), 3);
        assert_eq!(decode::<Article>(&bytes).unwrap(), articles);
    }

    #[test]
    fn record_types_are_not_confused() {
        let w = FixtureWriter::<EmbeddedArticle, _>::new(Vec::new()).unwrap();
        let bytes = w.out.clone();
        assert!(decode::<EmbeddedArticle>(&bytes).unwrap().is_empty());
        assert!(matches!(
            decode::<Article>(&bytes),
            Err(FixtureError::BadMagic)
        ));
        assert!(matches!(
            decode::<Article>(b"hello"),
            Err(FixtureError::BadMagic)
        ));
    }
}
