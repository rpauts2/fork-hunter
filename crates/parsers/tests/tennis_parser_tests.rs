// The TennisParser source is intentionally excluded from the compile path and
// is not a bookmaker adapter. Keep its registry row diagnostic-only until
// odds provenance and bookmaker attribution can be independently verified.
use parsers::parser_factory::ParserFactory;
use std::sync::Arc;

fn factory() -> ParserFactory {
    ParserFactory::new(Arc::new(
        reqwest::Client::builder().build().expect("client"),
    ))
}

#[test]
fn external_tennis_feed_is_not_registered_as_a_bookmaker() {
    let factory = factory();

    assert!(!factory.contains("tennis"));
    assert!(!factory.registered_slugs().contains(&"tennis".to_string()));
    assert!(!factory
        .get_enabled()
        .iter()
        .any(|parser| parser.slug() == "tennis"));
}

#[test]
fn tennis_feed_coverage_is_explicitly_not_production_enabled() {
    let coverage = factory()
        .parser_coverage()
        .into_iter()
        .find(|entry| entry.slug == "tennis")
        .expect("experimental tennis feed should remain visible in coverage");

    assert!(!coverage.enabled);
    assert!(!coverage.scan_supported);
    assert_eq!(coverage.parser_type, "external_feed");
    assert!(coverage
        .notes
        .as_deref()
        .unwrap_or_default()
        .contains("not a bookmaker"));
}
