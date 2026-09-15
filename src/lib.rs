use extism_pdk::{http, log, plugin_fn, FnResult, HttpRequest, Json, LogLevel, WithReturnCode};

use rs_plugin_common_interfaces::{
    domain::{external_images::ExternalImage, person::PersonType},
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
    TitleSearch { query: String, include_series: bool },
}

#[plugin_fn]
pub fn infos() -> FnResult<Json<PluginInformation>> {
    Ok(Json(PluginInformation {
        name: "libgen_source".into(),
        capabilities: vec![PluginType::LookupMetadata, PluginType::Lookup],
        version: env!("CARGO_PKG_VERSION_MINOR").parse()?,
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

    request.headers.insert(
        "Accept".to_string(),
        "text/html,application/xhtml+xml".to_string(),
    );
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
            if !targets
                .iter()
                .any(|t| matches!(t, LookupTarget::IsbnSearch(i) if *i == isbn))
            {
                targets.push(LookupTarget::IsbnSearch(isbn));
            }
        }
    }

    // Priority 3: Search the title/author index with every supported relation term.
    if let Some(search) = build_text_search(book) {
        targets.push(LookupTarget::TitleSearch {
            query: search,
            include_series: book
                .series
                .as_ref()
                .is_some_and(|series| !series.is_empty()),
        });
    }

    targets
}

fn build_text_search(book: &rs_plugin_common_interfaces::lookup::RsLookupBook) -> Option<String> {
    // Libgen exposes no searchable tag metadata. Returning no target avoids
    // silently broadening a filtered request.
    if book.tags.as_ref().is_some_and(|tags| !tags.is_empty()) {
        return None;
    }

    let mut terms = Vec::new();
    if let Some(name) = book
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && detect_isbn_query(name).is_none())
    {
        push_unique_term(&mut terms, name);
    }
    if let Some(author) = book
        .author
        .as_deref()
        .map(str::trim)
        .filter(|author| !author.is_empty())
    {
        push_unique_term(&mut terms, author);
    }

    for person in book.people.as_deref().unwrap_or_default() {
        if person
            .role
            .as_ref()
            .is_some_and(|role| role != &PersonType::Author)
        {
            return None;
        }
        let value = person
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                person
                    .ids
                    .as_ref()
                    .and_then(|ids| ids.get("libgen-author"))
                    .map(|value| value.replace('-', " "))
            })?;
        push_unique_term(&mut terms, &value);
    }

    for series in book.series.as_deref().unwrap_or_default() {
        let name = series
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())?;
        push_unique_term(&mut terms, name);
    }

    (!terms.is_empty()).then(|| terms.join(" "))
}

fn push_unique_term(terms: &mut Vec<String>, value: &str) {
    if !terms.iter().any(|term| term.eq_ignore_ascii_case(value)) {
        terms.push(value.to_string());
    }
}

fn book_matches_filters(
    query: &rs_plugin_common_interfaces::lookup::RsLookupBook,
    book: &LibgenBook,
) -> bool {
    if query.tags.as_ref().is_some_and(|tags| !tags.is_empty()) {
        return false;
    }

    let author_key = normalize_filter_value(&book.author);
    for person in query.people.as_deref().unwrap_or_default() {
        if person
            .role
            .as_ref()
            .is_some_and(|role| role != &PersonType::Author)
        {
            return false;
        }
        let name_matches = person.name.as_deref().is_some_and(|name| {
            let name = normalize_filter_value(name);
            !name.is_empty() && author_key.contains(&name)
        });
        let id_matches = person.ids.as_ref().is_some_and(|ids| {
            ids.get("libgen-author")
                .map(normalize_filter_value)
                .is_some_and(|id| !id.is_empty() && author_key.contains(&id))
        });
        if !name_matches && !id_matches {
            return false;
        }
    }

    let series_key = book.series.as_deref().map(normalize_filter_value);
    for series in query.series.as_deref().unwrap_or_default() {
        let name_matches = series.name.as_deref().is_some_and(|name| {
            let name = normalize_filter_value(name);
            !name.is_empty()
                && series_key
                    .as_deref()
                    .is_some_and(|series| series.contains(&name))
        });
        if !name_matches {
            return false;
        }
    }

    true
}

fn normalize_filter_value(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn filter_search_results(
    query: &rs_plugin_common_interfaces::lookup::RsLookupBook,
    mut books: Vec<LibgenBook>,
    next_page_key: Option<String>,
) -> (Vec<LibgenBook>, Option<String>) {
    books.retain(|book| book_matches_filters(query, book));
    (books, next_page_key)
}

fn execute_search(
    target: &LookupTarget,
    page: Option<u32>,
) -> FnResult<(Vec<LibgenBook>, Option<String>)> {
    let (query, column) = match target {
        LookupTarget::IsbnSearch(isbn) => (isbn.as_str(), SearchColumn::Isbn),
        LookupTarget::TitleSearch {
            query,
            include_series: true,
        } => (query.as_str(), SearchColumn::TitleAuthorSeries),
        LookupTarget::TitleSearch {
            query,
            include_series: false,
        } => (query.as_str(), SearchColumn::TitleAuthor),
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

    // Try each target in priority order until we get results. Keep a provider
    // pagination key even when stricter local relation checks empty this page.
    let mut filtered_page_next_key = None;
    for target in &targets {
        let match_type = match target {
            LookupTarget::IsbnSearch(_) => Some(RsLookupMatchType::ExactId),
            LookupTarget::TitleSearch { .. } => Some(RsLookupMatchType::ExactText),
        };

        let (books, next_page_key) = execute_search(target, page)?;
        let book_query = match &lookup.query {
            RsLookupQuery::Book(query) => query,
            _ => unreachable!("lookup targets are only produced for books"),
        };
        let (books, next_page_key) = filter_search_results(book_query, books, next_page_key);
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
        if filtered_page_next_key.is_none() {
            filtered_page_next_key = next_page_key;
        }
    }

    Ok(Json(RsLookupMetadataResults {
        results: vec![],
        next_page_key: filtered_page_next_key,
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
        let (books, next_page_key) = execute_search(target, None)?;
        let book_query = match &lookup.query {
            RsLookupQuery::Book(query) => query,
            _ => unreachable!("lookup targets are only produced for books"),
        };
        let (books, _) = filter_search_results(book_query, books, next_page_key);
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
    use rs_plugin_common_interfaces::lookup::{
        RsLookupBook, RsLookupMovie, RsLookupPersonFilter, RsLookupSerieFilter, RsLookupTagFilter,
    };

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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
            }),
            credential: None,
            params: None,
        };
        let targets = resolve_lookup_targets(&lookup);
        assert_eq!(targets.len(), 1);
        match &targets[0] {
            LookupTarget::TitleSearch {
                query,
                include_series: false,
            } => assert_eq!(query, "Changes Jim Butcher"),
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
                ..Default::default()
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
            LookupTarget::TitleSearch {
                query,
                include_series: false,
            } => assert_eq!(query, "Storm Front Jim Butcher"),
            _ => panic!("Expected title+author fallback"),
        }
    }

    #[test]
    fn resolve_targets_uses_people_and_series_names() {
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                name: Some("Dune".to_string()),
                people: Some(vec![RsLookupPersonFilter {
                    name: Some("Frank Herbert".to_string()),
                    role: None,
                    ..Default::default()
                }]),
                series: Some(vec![RsLookupSerieFilter {
                    name: Some("Dune Chronicles".to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            credential: None,
            params: None,
        };

        let targets = resolve_lookup_targets(&lookup);
        assert!(matches!(
            targets.as_slice(),
            [LookupTarget::TitleSearch { query, include_series: true }]
                if query == "Dune Frank Herbert Dune Chronicles"
        ));
    }

    #[test]
    fn resolve_targets_accepts_author_role_and_libgen_author_id() {
        let mut ids = rs_plugin_common_interfaces::domain::rs_ids::RsIds::default();
        ids.set("libgen-author", "octavia-e-butler");
        let lookup = RsLookupWrapper {
            query: RsLookupQuery::Book(RsLookupBook {
                people: Some(vec![RsLookupPersonFilter {
                    ids: Some(ids),
                    role: Some(PersonType::Author),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            credential: None,
            params: None,
        };

        assert!(matches!(
            resolve_lookup_targets(&lookup).as_slice(),
            [LookupTarget::TitleSearch { query, include_series: false }]
                if query == "octavia e butler"
        ));
    }

    #[test]
    fn resolve_targets_rejects_unsupported_roles_and_tags() {
        for book in [
            RsLookupBook {
                name: Some("Dune".to_string()),
                people: Some(vec![RsLookupPersonFilter {
                    name: Some("David Lynch".to_string()),
                    role: Some(PersonType::Director),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            RsLookupBook {
                name: Some("Dune".to_string()),
                tags: Some(vec![RsLookupTagFilter {
                    name: Some("Science Fiction".to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            },
        ] {
            let lookup = RsLookupWrapper {
                query: RsLookupQuery::Book(book),
                credential: None,
                params: None,
            };
            assert!(resolve_lookup_targets(&lookup).is_empty());
        }
    }

    #[test]
    fn filtered_empty_page_preserves_next_page_key() {
        let query = RsLookupBook {
            people: Some(vec![RsLookupPersonFilter {
                name: Some("Frank Herbert".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let books = vec![LibgenBook {
            title: "A different book".to_string(),
            author: "Another Author".to_string(),
            ..Default::default()
        }];

        let (filtered, next_page_key) = filter_search_results(&query, books, Some("2".to_string()));
        assert!(filtered.is_empty());
        assert_eq!(next_page_key.as_deref(), Some("2"));
    }
}
