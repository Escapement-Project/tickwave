//! A query's work in `tracing` spans. A binary of its own: tracing caches whether a span is
//! wanted for the whole process, and a test running beside this one, with no subscriber, would
//! settle it for spans this one has not reached yet.

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use common::{fixture, open};
use escapement_tickwave_view::{MarkerQuery, Query};

/// Records the name and level of every span opened.
#[derive(Default)]
struct Spans(Mutex<Vec<(&'static str, tracing::Level)>>);

impl tracing::Subscriber for Spans {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut seen = self.0.lock().unwrap();
        seen.push((span.metadata().name(), *span.metadata().level()));
        tracing::span::Id::from_u64(seen.len() as u64)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn a_query_measures_its_statistics_in_a_span() {
    let rec = open(&fixture("energy.tickwave"));
    let extent = rec.extent().unwrap();
    let q = Query {
        window: extent.clone(),
        columns: 200,
        channels: vec![0, 4],
        flags: Vec::new(),
        strip: None,
        statistics: vec![extent.clone(), extent.start..extent.start + 1000],
        markers: MarkerQuery::default(),
        aligned: false,
    };
    let cancel = AtomicBool::new(false);
    let spans = Arc::new(Spans::default());
    tracing::subscriber::with_default(Arc::clone(&spans), || {
        rec.query(&q, &cancel).unwrap();
    });
    let seen = spans.0.lock().unwrap().clone();
    for name in ["query", "channel", "statistics"] {
        assert!(
            seen.contains(&(name, tracing::Level::DEBUG)),
            "{name} in {seen:?}"
        );
    }
    // One for each interval the query measures.
    assert_eq!(seen.iter().filter(|s| s.0 == "statistics").count(), 2);
    assert!(
        seen.iter().all(|s| s.1 == tracing::Level::DEBUG),
        "{seen:?}"
    );
}
