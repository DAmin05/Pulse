"""The shared schemas compile for Python and round-trip."""

from pulse.v1 import article_pb2, embedder_pb2, embedder_pb2_grpc, story_pb2


def test_embedded_article_round_trip() -> None:
    msg = article_pb2.EmbeddedArticle(
        article=article_pb2.Article(
            id="abc",
            source_kind=article_pb2.SOURCE_KIND_RSS,
            title="Título",
            lang="es",
            published_at_ms=1_700_000_000_000,
        ),
        vector=[0.5, -0.25],
        model_version="test",
    )
    decoded = article_pb2.EmbeddedArticle.FromString(msg.SerializeToString())
    assert decoded == msg
    assert decoded.article.title == "Título"


def test_story_event_oneof() -> None:
    split = story_pb2.StorySplit(parent_story_id="s1", child_story_ids=["s2", "s3"])
    event = story_pb2.StoryEvent(event_id="e1", split=split)
    assert event.WhichOneof("kind") == "split"


def test_grpc_service_is_generated() -> None:
    assert hasattr(embedder_pb2_grpc, "EmbedderServiceServicer")
    assert embedder_pb2.EMBED_KIND_QUERY == 2
