//! Per-language mean centering of embeddings.
//!
//! Sentence embeddings are anisotropic: every vector shares a large common
//! component, so unrelated articles in the same language already score ~0.8
//! cosine, and each language has its own offset on top. That squeezes the
//! "same story" signal into a sliver near 1.0 and makes same-language topics
//! look more alike than cross-language reports of one event.
//!
//! Subtracting each language's mean vector (then re-normalizing) removes both
//! effects: unrelated pairs center on 0 regardless of language. The means are
//! fit once on a calibration set and frozen in a file tied to the model
//! version, never updated online, so the transform is identical on every
//! replay.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use pulse_core::proto::v1::EmbeddedArticle;
use serde::{Deserialize, Serialize};

use crate::ann::dot;

#[derive(Debug, Serialize, Deserialize)]
pub struct Centering {
    /// Means are only valid for vectors from this exact model.
    pub model_version: String,
    pub samples: usize,
    /// Fallback for languages without enough calibration samples.
    pub global: Vec<f32>,
    pub per_lang: BTreeMap<String, Vec<f32>>,
}

/// Conventional location of the frozen means for a model version, e.g.
/// `multilingual-e5-small@614241f/int8/t256` →
/// `config/centering/multilingual-e5-small-614241f-int8-t256.json`.
pub fn default_path(model_version: &str) -> std::path::PathBuf {
    let name: String = model_version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Path::new("config/centering").join(format!("{name}.json"))
}

impl Centering {
    /// Fits means from `records`. Languages with fewer than `min_samples`
    /// articles use the global mean.
    pub fn fit(records: &[EmbeddedArticle], min_samples: usize) -> Result<Self> {
        let first = records.first().context("no calibration records")?;
        let model_version = first.model_version.clone();
        let dim = first.vector.len();

        let mut global = vec![0f64; dim];
        let mut sums: BTreeMap<String, (Vec<f64>, usize)> = BTreeMap::new();
        for r in records {
            ensure!(
                r.model_version == model_version,
                "calibration mixes models: {} and {}",
                model_version,
                r.model_version
            );
            ensure!(r.vector.len() == dim, "calibration mixes dimensions");
            let lang = r.article.as_ref().map_or("und", |a| a.lang.as_str());
            let (sum, n) = sums
                .entry(lang.to_owned())
                .or_insert_with(|| (vec![0f64; dim], 0));
            for ((g, s), x) in global.iter_mut().zip(sum.iter_mut()).zip(&r.vector) {
                *g += f64::from(*x);
                *s += f64::from(*x);
            }
            *n += 1;
        }

        let mean = |sum: &[f64], n: usize| sum.iter().map(|s| (s / n as f64) as f32).collect();
        Ok(Self {
            model_version,
            samples: records.len(),
            global: mean(&global, records.len()),
            per_lang: sums
                .into_iter()
                .filter(|(_, (_, n))| *n >= min_samples)
                .map(|(lang, (sum, n))| (lang, mean(&sum, n)))
                .collect(),
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string(self)?)?;
        Ok(())
    }

    /// `normalize(v − mean[lang])`.
    pub fn apply(&self, lang: &str, vector: &[f32]) -> Vec<f32> {
        let mean = self.per_lang.get(lang).unwrap_or(&self.global);
        let mut out: Vec<f32> = vector.iter().zip(mean).map(|(x, m)| x - m).collect();
        let norm = dot(&out, &out).sqrt().max(1e-12);
        out.iter_mut().for_each(|x| *x /= norm);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulse_core::proto::v1::Article;

    fn rec(lang: &str, v: [f32; 3]) -> EmbeddedArticle {
        EmbeddedArticle {
            article: Some(Article {
                lang: lang.into(),
                ..Default::default()
            }),
            vector: v.to_vec(),
            model_version: "m".into(),
        }
    }

    #[test]
    fn removes_language_offset() {
        // Two languages with different shared components; the topic signal is
        // the last coordinate.
        let records = vec![
            rec("en", [1.0, 0.0, 0.2]),
            rec("en", [1.0, 0.0, -0.2]),
            rec("es", [0.0, 1.0, 0.2]),
            rec("es", [0.0, 1.0, -0.2]),
            rec("fr", [0.5, 0.5, 0.0]),
        ];
        let c = Centering::fit(&records, 2).unwrap();
        assert!(c.per_lang.contains_key("en") && !c.per_lang.contains_key("fr"));

        // Same topic across languages: dissimilar raw, identical after centering.
        let en = c.apply("en", &[1.0, 0.0, 0.2]);
        let es = c.apply("es", &[0.0, 1.0, 0.2]);
        assert!(dot(&en, &es) > 0.99);
        // Opposite topics in one language: opposite after centering.
        assert!(dot(&en, &c.apply("en", &[1.0, 0.0, -0.2])) < -0.99);
        // Unknown language falls back to the global mean; output stays unit length.
        let fr = c.apply("xx", &[0.5, 0.5, 0.3]);
        assert!((dot(&fr, &fr) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn default_path_is_stable() {
        assert_eq!(
            default_path("multilingual-e5-small@614241f/int8/t256"),
            Path::new("config/centering/multilingual-e5-small-614241f-int8-t256.json")
        );
    }

    #[test]
    fn rejects_mixed_models() {
        let mut other = rec("en", [1.0, 0.0, 0.0]);
        other.model_version = "other".into();
        assert!(Centering::fit(&[rec("en", [1.0, 0.0, 0.0]), other], 1).is_err());
    }
}
