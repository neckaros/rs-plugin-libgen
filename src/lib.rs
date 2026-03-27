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
        version: 3,
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

fn resolve_lookup_target(lookup: &RsLookupWrapper) -> Option<LookupTarget> {
    let book = match &lookup.query {
        RsLookupQuery::Book(book) => book,
        _ => return None,
    };

    // Check IDs for ISBN13
    if let Some(ids) = book.ids.as_ref() {
        if let Some(isbn) = ids.isbn13() {
            let compact: String = isbn.chars().filter(|c| c.is_ascii_digit()).collect();
            if compact.len() == 13 {
                return Some(LookupTarget::IsbnSearch(compact));
            }
        }
    }

    // Check if name looks like an ISBN
    if let Some(name) = book.name.as_deref() {
        if let Some(isbn) = detect_isbn_query(name) {
            return Some(LookupTarget::IsbnSearch(isbn));
        }
    }

    // Fall back to title search
    book.name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| LookupTarget::TitleSearch(n.to_string()))
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
    let target = match resolve_lookup_target(&lookup) {
        Some(t) => t,
        None => {
            return Ok(Json(RsLookupMetadataResults {
                results: vec![],
                next_page_key: None,
            }))
        }
    };

    let page = match &lookup.query {
        RsLookupQuery::Book(book) => book.page_key.as_deref().and_then(|k| k.parse::<u32>().ok()),
        _ => None,
    };

    let match_type = match &target {
        LookupTarget::IsbnSearch(_) => Some(RsLookupMatchType::ExactId),
        LookupTarget::TitleSearch(_) => Some(RsLookupMatchType::ExactText),
    };

    let (books, next_page_key) = execute_search(&target, page)?;

    let results = books
        .into_iter()
        .map(|book| libgen_book_to_result(book, match_type.clone()))
        .collect();

    Ok(Json(RsLookupMetadataResults {
        results,
        next_page_key,
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
    let target = match resolve_lookup_target(&lookup) {
        Some(t) => t,
        None => return Ok(Json(RsLookupSourceResult::NotApplicable)),
    };

    let match_type = match &target {
        LookupTarget::IsbnSearch(_) => Some(RsLookupMatchType::ExactId),
        LookupTarget::TitleSearch(_) => None,
    };

    let (books, _) = execute_search(&target, None)?;

    if books.is_empty() {
        return Ok(Json(RsLookupSourceResult::NotFound));
    }

    // Resolve download URLs for the top results (limit HTTP round-trips)
    let max_resolve = 5.min(books.len());
    let mut requests: Vec<RsRequest> = Vec::new();

    for book in books.iter().take(max_resolve) {
        if let Some(md5) = &book.md5 {
            if let Some(download_url) = resolve_download_url(md5) {
                let mut req = libgen_book_to_request(book, download_url);
                if let Some(mt) = &match_type {
                    // Store match info in description for the host
                    if req.title.is_none() {
                        req.title = Some(book.title.clone());
                    }
                    let _ = mt; // match_type is used at the result level, not per-request
                }
                requests.push(req);
            }
        }
    }

    if requests.is_empty() {
        Ok(Json(RsLookupSourceResult::NotFound))
    } else {
        Ok(Json(RsLookupSourceResult::Requests(requests)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_plugin_common_interfaces::lookup::{RsLookupBook, RsLookupMovie};

    #[test]
    fn resolve_target_non_book_returns_none() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Movie(RsLookupMovie::default()),
            credential: None,
            params: None,
        };
        assert!(resolve_lookup_target(&lookup).is_none());
    }

    #[test]
    fn resolve_target_empty_name_returns_none() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some(String::new()),
                ids: None,
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        assert!(resolve_lookup_target(&lookup).is_none());
    }

    #[test]
    fn resolve_target_isbn_in_name() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some("9780451457813".to_string()),
                ids: None,
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        match resolve_lookup_target(&lookup) {
            Some(LookupTarget::IsbnSearch(isbn)) => assert_eq!(isbn, "9780451457813"),
            _ => panic!("Expected ISBN search"),
        }
    }

    #[test]
    fn resolve_target_title_search() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some("Storm Front".to_string()),
                ids: None,
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        match resolve_lookup_target(&lookup) {
            Some(LookupTarget::TitleSearch(title)) => assert_eq!(title, "Storm Front"),
            _ => panic!("Expected title search"),
        }
    }

    #[test]
    fn resolve_target_isbn_from_ids() {
        let mut ids = rs_plugin_common_interfaces::domain::rs_ids::RsIds::default();
        ids.set("isbn13", "9780451457813");
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some("Storm Front".to_string()),
                ids: Some(ids),
                page_key: None,
            }),
            credential: None,
            params: None,
        };
        match resolve_lookup_target(&lookup) {
            Some(LookupTarget::IsbnSearch(isbn)) => assert_eq!(isbn, "9780451457813"),
            _ => panic!("Expected ISBN search from ids"),
        }
    }
}
