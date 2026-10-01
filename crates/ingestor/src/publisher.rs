//! Publishes articles to `articles.raw` with an idempotent producer.

use std::time::Duration;

use anyhow::Result;
use prost::Message;
use pulse_core::proto::v1::Article;
use pulse_core::{kafka, topics};
use rdkafka::message::{Header, OwnedHeaders};
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use rdkafka::util::Timeout;

pub const CONTENT_TYPE: &str = "application/x-protobuf; messageType=pulse.v1.Article";

pub struct Publisher {
    producer: FutureProducer,
}

impl Publisher {
    pub fn new(brokers: &str) -> Result<Self> {
        Ok(Self {
            producer: kafka::idempotent_producer(brokers, "ingestor").create()?,
        })
    }

    pub async fn publish(&self, article: &Article) -> Result<()> {
        let payload = article.encode_to_vec();
        let record = FutureRecord::to(topics::ARTICLES_RAW)
            .key(&article.id)
            .payload(&payload)
            .headers(OwnedHeaders::new().insert(Header {
                key: "content-type",
                value: Some(CONTENT_TYPE),
            }));
        self.producer
            .send(record, Timeout::After(Duration::from_secs(30)))
            .await
            .map_err(|(e, _)| e)?;
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        self.producer
            .flush(Timeout::After(Duration::from_secs(10)))?;
        Ok(())
    }
}
