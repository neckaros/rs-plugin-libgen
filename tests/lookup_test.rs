use extism::{Manifest, Plugin, Wasm};
use rs_plugin_common_interfaces::{
    lookup::{
        RsLookupBook, RsLookupMetadataResults, RsLookupQuery, RsLookupSourceResult,
        RsLookupWrapper,
    },
    PluginInformation,
};

fn build_plugin() -> Plugin {
    let wasm = Wasm::file("target/wasm32-unknown-unknown/release/rs_plugin_libgen.wasm");
    let manifest = Manifest::new([wasm]).with_allowed_host("libgen.bz");
    Plugin::new(&manifest, [], true).expect("Failed to create plugin")
}

#[test]
fn test_infos() {
    let mut plugin = build_plugin();
    let res = plugin
        .call::<&str, String>("infos", "")
        .expect("infos call failed");
    let info: PluginInformation = serde_json::from_str(&res).expect("Failed to parse infos");
    assert_eq!(info.name, "libgen_source");
    assert_eq!(info.capabilities.len(), 2);
}

#[test]
fn test_lookup_metadata_title_search() {
    let mut plugin = build_plugin();
    let input = RsLookupWrapper {
        query: RsLookupQuery::Book(RsLookupBook {
            name: Some("dune frank herbert".to_string()),
            author: None,
            ids: None,
            page_key: None,
        }),
        credential: None,
        params: None,
    };

    let input_json = serde_json::to_string(&input).unwrap();
    let res = plugin
        .call::<&str, String>("lookup_metadata", &input_json)
        .expect("lookup_metadata call failed");
    let results: RsLookupMetadataResults =
        serde_json::from_str(&res).expect("Failed to parse results");

    println!("Found {} results for 'dune frank herbert'", results.results.len());
    assert!(!results.results.is_empty(), "Expected at least one result");

    for (i, r) in results.results.iter().take(3).enumerate() {
        println!("  [{}] {:?}", i, r.metadata);
    }
}

#[test]
fn test_lookup_non_book_returns_not_applicable() {
    let mut plugin = build_plugin();
    let input = RsLookupWrapper {
        query: RsLookupQuery::Movie(Default::default()),
        credential: None,
        params: None,
    };

    let input_json = serde_json::to_string(&input).unwrap();
    let res = plugin
        .call::<&str, String>("lookup", &input_json)
        .expect("lookup call failed");
    let result: RsLookupSourceResult =
        serde_json::from_str(&res).expect("Failed to parse result");

    assert!(
        matches!(result, RsLookupSourceResult::NotApplicable),
        "Expected NotApplicable for Movie query"
    );
}

#[test]
fn test_lookup_returns_download_requests() {
    let mut plugin = build_plugin();
    let input = RsLookupWrapper {
        query: RsLookupQuery::Book(RsLookupBook {
            name: Some("storm front jim butcher".to_string()),
            author: None,
            ids: None,
            page_key: None,
        }),
        credential: None,
        params: None,
    };

    let input_json = serde_json::to_string(&input).unwrap();
    let res = plugin
        .call::<&str, String>("lookup", &input_json)
        .expect("lookup call failed");
    let result: RsLookupSourceResult =
        serde_json::from_str(&res).expect("Failed to parse result");

    match result {
        RsLookupSourceResult::Requests(requests) => {
            println!("Got {} download requests", requests.len());
            for (i, req) in requests.iter().enumerate() {
                println!(
                    "  [{}] {} | mime={:?} | size={:?} | file={:?}",
                    i, req.url, req.mime, req.size, req.filename
                );
            }
            assert!(!requests.is_empty(), "Expected at least one download request");
            assert!(
                requests[0].url.contains("libgen.bz"),
                "Expected libgen.bz download URL"
            );
        }
        other => panic!("Expected Requests, got {:?}", other),
    }
}
