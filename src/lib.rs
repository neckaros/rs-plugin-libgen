use extism_pdk::{http, log, plugin_fn, FnResult, HttpRequest, Json, LogLevel, WithReturnCode};

use rs_plugin_common_interfaces::{
    domain::external_images::ExternalImage,
    lookup::{
        RsLookupMatchType, RsLookupMetadataResults, RsLookupQuery, RsLookupSourceResult,
        RsLookupWrapper,
    },
    request::RsRequest,
    PluginInformation, PluginType,
};

mod convert;
mod libgen;

use convert::{libgen_book_to_request, libgen_book_to_result};
use libgen::{
    build_download_page_url, build_download_url, build_search_url, detect_isbn_query,
    format_priority, parse_download_key, parse_search_html, parse_search_next_page, LibgenBook,
    SearchColumn,
};

enum LookupTarget {
    IsbnSearch(String),
    TitleSearch(String),
}

#[plugin_fn]
pub fn infos() -> FnResult<Json<PluginInformation>> {
    Ok(Json(PluginInformation {
        name: "libgen_source".into(),
        capabilities: vec![PluginType::LookupMetadata, PluginType::Lookup],
        version: 4,
        interface_version: 1,
        repo: Some("https://github.com/neckaros/rs-plugin-libgen".to_string()),
        publisher: "neckaros".into(),
        description: "Search and download books from Library Genesis".into(),
        credential_kind: None,
        settings: vec![],
        ..Default::default()
    }))
}

fn build_http_request(url: String) -> HttpRequest {
    let mut request = HttpRequest {
        url,
        headers: Default::default(),
        method: Some("GET".into()),
    };

    request
        .headers
        .insert("Accept".to_string(), "text/html,application/xhtml+xml".to_string());
    request.headers.insert(
        "User-Agent".to_string(),
        "Mozilla/5.0 (compatible; rs-plugin-libgen/0.1)".to_string(),
    );

    request
}

fn execute_html_request(url: String) -> FnResult<String> {
    let request = build_http_request(url);
    let res = http::request::<Vec<u8>>(&request, None);

    match res {
        Ok(res) if res.status_code() >= 200 && res.status_code() < 300 => {
            Ok(String::from_utf8_lossy(&res.body()).to_string())
        }
        Ok(res) => {
            log!(
                LogLevel::Error,
                "libgen HTTP error {}: {}",
                res.status_code(),
                String::from_utf8_lossy(&res.body())
            );
            Err(WithReturnCode::new(
                extism_pdk::Error::msg(format!("HTTP error: {}", res.status_code())),
                res.status_code() as i32,
            ))
        }
        Err(e) => {
            log!(LogLevel::Error, "libgen request failed: {}", e);
            Err(WithReturnCode(e, 500))
        }
    }
}

/// Build a prioritized list of lookup targets to try in order.
/// ISBN is most precise and tried first, then title+author as fallback.
fn resolve_lookup_targets(lookup: &RsLookupWrapper) -> Vec<LookupTarget> {
    let book = match &lookup.query {
        RsLookupQuery::Book(book) => book,
        _ => return vec![],
    };

    let mut targets = Vec::new();

    // Priority 1: ISBN from ids
    if let Some(ids) = book.ids.as_ref() {
        if let Some(isbn) = ids.isbn13() {
            let compact: String = isbn.chars().filter(|c| c.is_ascii_digit()).collect();
            if compact.len() == 13 {
                targets.push(LookupTarget::IsbnSearch(compact));
            }
        }
    }

    // Priority 2: ISBN detected in name
    if let Some(name) = book.name.as_deref() {
        if let Some(isbn) = detect_isbn_query(name) {
            if !targets.iter().any(|t| matches!(t, LookupTarget::IsbnSearch(i) if *i == isbn)) {
                targets.push(LookupTarget::IsbnSearch(isbn));
            }
        }
    }

    // Priority 3: Title + author search
    if let Some(name) = book.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        if detect_isbn_query(name).is_none() {
            let search = match book.author.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
                Some(author) => format!("{name} {author}"),
                None => name.to_string(),
            };
            targets.push(LookupTarget::TitleSearch(search));
        }
    }

    targets
}

fn execute_search(
    target: &LookupTarget,
    page: Option<u32>,
) -> FnResult<(Vec<LibgenBook>, Option<String>)> {
    let (query, column) = match target {
        LookupTarget::IsbnSearch(isbn) => (isbn.as_str(), SearchColumn::Isbn),
        LookupTarget::TitleSearch(title) => (title.as_str(), SearchColumn::TitleAuthor),
    };

    let url = build_search_url(query, page, &column)
        .ok_or_else(|| WithReturnCode::new(extism_pdk::Error::msg("Not supported"), 404))?;

    let body = execute_html_request(url)?;
    let mut books = parse_search_html(&body);

    // Sort by format preference
    books.sort_by_key(|b| format_priority(&b.extension));

    let current_page = page.unwrap_or(1);
    let next_page_key = if books.is_empty() {
        None
    } else {
        parse_search_next_page(&body, current_page).map(|p| p.to_string())
    };

    Ok((books, next_page_key))
}

fn resolve_download_url(md5: &str) -> Option<String> {
    let page_url = build_download_page_url(md5);
    match execute_html_request(page_url) {
        Ok(html) => {
            if let Some(key) = parse_download_key(&html) {
                Some(build_download_url(md5, &key))
            } else {
                // Fallback: return the intermediate page URL
                Some(build_download_page_url(md5))
            }
        }
        Err(_) => {
            // Fallback to intermediate URL on error
            Some(build_download_page_url(md5))
        }
    }
}

#[plugin_fn]
pub fn lookup_metadata(
    Json(lookup): Json<RsLookupWrapper>,
) -> FnResult<Json<RsLookupMetadataResults>> {
    let targets = resolve_lookup_targets(&lookup);
    if targets.is_empty() {
        return Ok(Json(RsLookupMetadataResults {
            results: vec![],
            next_page_key: None,
        }));
    }

    let page = match &lookup.query {
        RsLookupQuery::Book(book) => book.page_key.as_deref().and_then(|k| k.parse::<u32>().ok()),
        _ => None,
    };

    // Try each target in priority order until we get results
    for target in &targets {
        let match_type = match target {
            LookupTarget::IsbnSearch(_) => Some(RsLookupMatchType::ExactId),
            LookupTarget::TitleSearch(_) => Some(RsLookupMatchType::ExactText),
        };

        let (books, next_page_key) = execute_search(target, page)?;
        if !books.is_empty() {
            let results = books
                .into_iter()
                .map(|book| libgen_book_to_result(book, match_type.clone()))
                .collect();
            return Ok(Json(RsLookupMetadataResults {
                results,
                next_page_key,
            }));
        }
    }

    Ok(Json(RsLookupMetadataResults {
        results: vec![],
        next_page_key: None,
    }))
}

#[plugin_fn]
pub fn lookup_metadata_images(
    Json(_lookup): Json<RsLookupWrapper>,
) -> FnResult<Json<Vec<ExternalImage>>> {
    // Libgen search results don't include cover images
    Ok(Json(vec![]))
}

#[plugin_fn]
pub fn lookup(Json(lookup): Json<RsLookupWrapper>) -> FnResult<Json<RsLookupSourceResult>> {
    let targets = resolve_lookup_targets(&lookup);
    if targets.is_empty() {
        return Ok(Json(RsLookupSourceResult::NotApplicable));
    }

    // Try each target in priority order until we get results
    for target in &targets {
        let (books, _) = execute_search(target, None)?;
        if books.is_empty() {
            continue;
        }

        // Resolve download URLs for the top results (limit HTTP round-trips)
        let max_resolve = 5.min(books.len());
        let mut requests: Vec<RsRequest> = Vec::new();

        for book in books.iter().take(max_resolve) {
            if let Some(md5) = &book.md5 {
                if let Some(download_url) = resolve_download_url(md5) {
                    requests.push(libgen_book_to_request(book, download_url));
                }
            }
        }

        if !requests.is_empty() {
            return Ok(Json(RsLookupSourceResult::Requests(requests)));
        }
    }

    Ok(Json(RsLookupSourceResult::NotFound))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_plugin_common_interfaces::lookup::{RsLookupBook, RsLookupMovie};

    #[test]
    fn resolve_targets_non_book_returns_empty() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Movie(RsLookupMovie::default()),
            credential: None,
            params: None,
        };
        assert!(resolve_lookup_targets(&lookup).is_empty());
    }

    #[test]
    fn resolve_targets_empty_name_returns_empty() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some(String::new()),
                author: None,
                ids: None,
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        assert!(resolve_lookup_targets(&lookup).is_empty());
    }

    #[test]
    fn resolve_targets_isbn_in_name() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some("9780451457813".to_string()),
                author: None,
                ids: None,
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        let targets = resolve_lookup_targets(&lookup);
        assert_eq!(targets.len(), 1);
        match &targets[0] {
            LookupTarget::IsbnSearch(isbn) => assert_eq!(isbn, "9780451457813"),
            _ => panic!("Expected ISBN search"),
        }
    }

    #[test]
    fn resolve_targets_title_with_author() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some("Changes".to_string()),
                author: Some("Jim Butcher".to_string()),
                ids: None,
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        let targets = resolve_lookup_targets(&lookup);
        assert_eq!(targets.len(), 1);
        match &targets[0] {
            LookupTarget::TitleSearch(search) => assert_eq!(search, "Changes Jim Butcher"),
            _ => panic!("Expected title search with author"),
        }
    }

    #[test]
    fn resolve_targets_isbn_from_ids_with_title_fallback() {
        let mut ids = rs_plugin_common_interfaces::domain::rs_ids::RsIds::default();
        ids.set("isbn13", "9780451457813");
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some("Storm Front".to_string()),
                author: Some("Jim Butcher".to_string()),
                ids: Some(ids),
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        let targets = resolve_lookup_targets(&lookup);
        assert_eq!(targets.len(), 2);
        match &targets[0] {
            LookupTarget::IsbnSearch(isbn) => assert_eq!(isbn, "9780451457813"),
            _ => panic!("Expected ISBN first"),
        }
        match &targets[1] {
            LookupTarget::TitleSearch(search) => assert_eq!(search, "Storm Front Jim Butcher"),
            _ => panic!("Expected title+author fallback"),
        }
    }
}
