//! Where briefing audio lives: S3 (SeaweedFS locally) or a local directory.

use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pulse_core::config::{Settings, env_or};

pub struct AudioStore {
    inner: Arc<dyn ObjectStore>,
    pub description: String,
}

impl AudioStore {
    /// `PULSE_AUDIO_STORE=s3` (default; bucket `PULSE_AUDIO_BUCKET`) or `local`
    /// (directory `PULSE_AUDIO_DIR`).
    pub fn from_env(settings: &Settings) -> Result<Self> {
        if env_or("PULSE_AUDIO_STORE", "s3") == "local" {
            let dir = env_or("PULSE_AUDIO_DIR", "data/audio");
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
            return Ok(Self {
                inner: Arc::new(LocalFileSystem::new_with_prefix(&dir)?),
                description: format!("local:{dir}"),
            });
        }
        let bucket = env_or("PULSE_AUDIO_BUCKET", "pulse-audio");
        let s3 = AmazonS3Builder::new()
            .with_endpoint(&settings.s3_endpoint)
            .with_bucket_name(&bucket)
            .with_region(env_or("PULSE_S3_REGION", "us-east-1"))
            .with_access_key_id(env_or("PULSE_S3_ACCESS_KEY", "pulse"))
            .with_secret_access_key(env_or("PULSE_S3_SECRET_KEY", "pulse-dev-secret"))
            .with_allow_http(settings.s3_endpoint.starts_with("http://"))
            .with_virtual_hosted_style_request(false)
            .build()
            .context("configuring the audio store")?;
        Ok(Self {
            inner: Arc::new(s3),
            description: format!("s3://{bucket}"),
        })
    }

    fn path(key: &str) -> Path {
        Path::from(format!("briefings/{key}"))
    }

    pub async fn put(&self, key: &str, audio: Bytes) -> Result<()> {
        self.inner
            .put(&Self::path(key), PutPayload::from(audio))
            .await
            .with_context(|| format!("storing audio in {}", self.description))?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Bytes> {
        Ok(self.inner.get(&Self::path(key)).await?.bytes().await?)
    }
}
