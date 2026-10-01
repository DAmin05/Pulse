//! gRPC client for the Embedder: concurrent, order-preserving, with retries.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::{StreamExt, TryStreamExt, stream};
use metrics::counter;
use pulse_core::proto::v1::embedder_service_client::EmbedderServiceClient;
use pulse_core::proto::v1::{EmbedKind, EmbedRequest, GetModelInfoRequest, GetModelInfoResponse};
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request};

const ATTEMPTS: u32 = 6;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Embedder {
    client: EmbedderServiceClient<Channel>,
    request_size: usize,
    concurrency: usize,
}

pub struct Embedded {
    pub vectors: Vec<Vec<f32>>,
    pub model_version: String,
}

impl Embedder {
    pub fn connect_lazy(url: &str, request_size: usize, concurrency: usize) -> Result<Self> {
        let channel = Endpoint::from_shared(url.to_owned())
            .with_context(|| format!("invalid embedder url {url}"))?
            .connect_timeout(Duration::from_secs(5))
            .connect_lazy();
        Ok(Self {
            client: EmbedderServiceClient::new(channel).max_decoding_message_size(16 * 1024 * 1024),
            request_size,
            concurrency,
        })
    }

    pub async fn model_info(&self) -> Result<GetModelInfoResponse> {
        with_retry("model_info", || {
            let mut c = self.client.clone();
            async move { c.get_model_info(GetModelInfoRequest {}).await }
        })
        .await
    }

    /// Embeds passages, returning vectors in input order. Sends `request_size`
    /// texts per RPC with up to `concurrency` RPCs in flight, so the server's
    /// dynamic batcher can merge them.
    pub async fn embed_passages(&self, texts: Vec<String>) -> Result<Embedded> {
        let expected = texts.len();
        let chunks: Vec<Vec<String>> = texts
            .chunks(self.request_size)
            .map(<[String]>::to_vec)
            .collect();

        let responses: Vec<_> = stream::iter(chunks)
            .map(|chunk| {
                let client = self.client.clone();
                async move {
                    with_retry("embed", || {
                        let mut c = client.clone();
                        let mut req = Request::new(EmbedRequest {
                            texts: chunk.clone(),
                            kind: EmbedKind::Passage as i32,
                        });
                        req.set_timeout(REQUEST_TIMEOUT);
                        async move { c.embed(req).await }
                    })
                    .await
                }
            })
            .buffered(self.concurrency)
            .try_collect()
            .await?;

        let model_version = responses
            .first()
            .map(|r| r.model_version.clone())
            .unwrap_or_default();
        if responses.iter().any(|r| r.model_version != model_version) {
            // Mixing models inside one batch would make the log inconsistent.
            bail!("embedder model changed mid-batch");
        }
        let vectors: Vec<Vec<f32>> = responses
            .into_iter()
            .flat_map(|r| r.vectors.into_iter().map(|v| v.values))
            .collect();
        if vectors.len() != expected {
            bail!(
                "embedder returned {} vectors for {expected} texts",
                vectors.len()
            );
        }
        Ok(Embedded {
            vectors,
            model_version,
        })
    }
}

fn retryable(code: Code) -> bool {
    matches!(
        code,
        Code::Unavailable | Code::DeadlineExceeded | Code::ResourceExhausted | Code::Aborted
    )
}

async fn with_retry<T, F, Fut>(op: &'static str, mut call: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<tonic::Response<T>, tonic::Status>>,
{
    let mut delay = Duration::from_millis(500);
    for attempt in 1..=ATTEMPTS {
        match call().await {
            Ok(resp) => return Ok(resp.into_inner()),
            Err(status) if retryable(status.code()) && attempt < ATTEMPTS => {
                counter!("pulse_relay_embed_retries_total").increment(1);
                tracing::warn!(op, attempt, code = ?status.code(), message = status.message(), retry_in = ?delay, "embedder call failed");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(10));
            }
            Err(status) => return Err(status).with_context(|| format!("embedder {op}")),
        }
    }
    unreachable!("loop returns on the last attempt")
}
