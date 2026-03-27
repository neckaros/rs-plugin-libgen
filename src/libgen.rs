use regex::Regex;
use scraper::{Html, Selector};

#[derive(Debug, Clone, Default)]
pub struct LibgenBook {
    pub file_id: Option<String>,
    pub md5: Option<String>,
    pub title: String,
    pub author: String,
    pub series: Option<String>,
    pub publisher: String,
    pub year: Option<u16>,
    pub language: String,
    pub pages: Option<u32>,
    pub size_bytes: Option<u64>,
    pub extension: String,
    pub isbn: Option<String>,
}

pub enum SearchColumn {
    TitleAuthor,
    Isbn,
}

pub fn encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for b in value.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*b as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{:02X}", b)),
        }
    }
    encoded
}

pub fn build_search_url(query: &str, page: Option<u32>, column: &SearchColumn) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }

    let encoded = encode_query_component(trimmed);

    let columns = match column {
        SearchColumn::TitleAuthor => "columns%5B%5D=t&columns%5B%5D=a",
        SearchColumn::Isbn => "columns%5B%5D=i",
    };

    let page_num = page.unwrap_or(1);

    Some(format!(
        "https://libgen.bz/index.php?req={encoded}&{columns}&objects%5B%5D=f&topics%5B%5D=l&topics%5B%5D=f&res=25&page={page_num}"
    ))
}

pub fn build_download_page_url(md5: &str) -> String {
    format!("https://libgen.bz/get.php?md5={md5}")
}

pub fn build_download_url(md5: &str, key: &str) -> String {
    format!("https://libgen.bz/get.php?md5={md5}&key={key}")
}

pub fn parse_search_html(html: &str) -> Vec<LibgenBook> {
    let document = Html::parse_document(html);
    let mut books = Vec::new();

    let table_sel = Selector::parse("#tablelibgen").unwrap();
    let Some(table) = document.select(&table_sel).next() else {
        return books;
    };

    let tbody_sel = Selector::parse("tbody").unwrap();
    let row_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let a_sel = Selector::parse("a").unwrap();
    let b_sel = Selector::parse("b").unwrap();
    let font_green_sel = Selector::parse("font[color=\"green\"]").unwrap();
    let badge_secondary_sel = Selector::parse("span.badge-secondary").unwrap();

    let md5_re = Regex::new(r"md5=([a-fA-F0-9]{32})").unwrap();
    let isbn13_re = Regex::new(r"\b(\d{13})\b").unwrap();
    let file_id_re = Regex::new(r"[flbcsrm]\s+(\d+)").unwrap();

    let rows_container = table
        .select(&tbody_sel)
        .next()
        .map(|tbody| tbody.select(&row_sel).collect::<Vec<_>>())
        .unwrap_or_else(|| table.select(&row_sel).skip(1).collect());

    for row in &rows_container {
        let cells: Vec<_> = row.select(&td_sel).collect();
        // Layout: [Title/Series/ID cell] [Author] [Publisher] [Year] [Language] [Pages] [Size] [Ext] [Mirrors]
        if cells.len() < 9 {
            continue;
        }

        let info_cell = &cells[0];

        // Extract title from main <a> links to edition.php (skip the bold series text)
        let title = info_cell
            .select(&a_sel)
            .filter(|a| {
                a.value()
                    .attr("href")
                    .map(|h| h.starts_with("edition.php"))
                    .unwrap_or(false)
            })
            .map(|a| a.text().collect::<String>())
            .find(|t| {
                let trimmed = t.trim();
                !trimmed.is_empty()
                    && !trimmed.chars().all(|c| c.is_ascii_digit() || c == ';' || c.is_whitespace())
            })
            .map(|t| t.trim().to_string())
            .unwrap_or_default();

        if title.is_empty() {
            continue;
        }

        // Extract series from <b> tag (if present)
        let series = info_cell
            .select(&b_sel)
            .next()
            .map(|b| b.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty());

        // Extract ISBN from green font
        let isbn = info_cell
            .select(&font_green_sel)
            .next()
            .and_then(|font| {
                let text = font.text().collect::<String>();
                isbn13_re.find(&text).map(|m| m.as_str().to_string())
            });

        // Extract file ID from badge-secondary span
        let file_id = info_cell
            .select(&badge_secondary_sel)
            .next()
            .and_then(|span| {
                let text = span.text().collect::<String>();
                file_id_re
                    .captures(&text)
                    .map(|cap| cap[1].to_string())
            });

        let author = cells[1].text().collect::<String>().trim().to_string();
        let publisher = cells[2].text().collect::<String>().trim().to_string();

        let year_text = cells[3].text().collect::<String>().trim().to_string();
        let year = year_text.parse::<u16>().ok().filter(|y| *y >= 1000 && *y <= 2999);

        let language = cells[4].text().collect::<String>().trim().to_string();

        let pages_text = cells[5].text().collect::<String>().trim().to_string();
        let pages = pages_text
            .replace('[', "")
            .replace(']', "")
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|p| *p > 0);

        let size_text = cells[6].text().collect::<String>().trim().to_string();
        let size_bytes = parse_size_to_bytes(&size_text);

        let extension = cells[7].text().collect::<String>().trim().to_lowercase();

        // Extract MD5 from mirror links in the last cell
        let md5 = cells[8]
            .select(&a_sel)
            .find_map(|a| {
                a.value()
                    .attr("href")
                    .and_then(|href| md5_re.captures(href))
                    .map(|cap| cap[1].to_lowercase())
            });

        if md5.is_none() {
            continue;
        }

        books.push(LibgenBook {
            file_id,
            md5,
            title,
            author,
            series,
            publisher,
            year,
            language,
            pages,
            size_bytes,
            extension,
            isbn,
        });
    }

    books
}

pub fn parse_search_next_page(html: &str, current_page: u32) -> Option<u32> {
    let document = Html::parse_document(html);
    let a_sel = Selector::parse("a").unwrap();
    let page_re = Regex::new(r"[?&]page=(\d+)").unwrap();

    let mut max_page = current_page;
    for a in document.select(&a_sel) {
        if let Some(href) = a.value().attr("href") {
            if let Some(cap) = page_re.captures(href) {
                if let Ok(p) = cap[1].parse::<u32>() {
                    if p > max_page {
                        max_page = p;
                    }
                }
            }
        }
    }

    if max_page > current_page {
        Some(current_page + 1)
    } else {
        None
    }
}

pub fn parse_download_key(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let a_sel = Selector::parse("a").unwrap();
    let key_re = Regex::new(r"[?&]key=([A-Za-z0-9]+)").unwrap();

    for a in document.select(&a_sel) {
        if let Some(href) = a.value().attr("href") {
            if href.contains("get.php") || href.contains("md5=") {
                if let Some(cap) = key_re.captures(href) {
                    return Some(cap[1].to_string());
                }
            }
        }
    }

    None
}

pub fn parse_size_to_bytes(size_str: &str) -> Option<u64> {
    let trimmed = size_str.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_lowercase();
    let re = Regex::new(r"^([\d.]+)\s*(kb|mb|gb|tb|bytes?|b)$").unwrap();

    if let Some(cap) = re.captures(&lower) {
        let value: f64 = cap[1].parse().ok()?;
        let multiplier: f64 = match &cap[2] {
            "b" | "byte" | "bytes" => 1.0,
            "kb" => 1024.0,
            "mb" => 1024.0 * 1024.0,
            "gb" => 1024.0 * 1024.0 * 1024.0,
            "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
            _ => return None,
        };
        Some((value * multiplier) as u64)
    } else {
        None
    }
}

pub fn extension_to_mime(ext: &str) -> Option<String> {
    match ext.to_lowercase().as_str() {
        "epub" => Some("application/epub+zip".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "mobi" => Some("application/x-mobipocket-ebook".to_string()),
        "azw3" | "azw" => Some("application/vnd.amazon.ebook".to_string()),
        "djvu" => Some("image/vnd.djvu".to_string()),
        "fb2" => Some("application/x-fictionbook+xml".to_string()),
        "txt" => Some("text/plain".to_string()),
        "rtf" => Some("application/rtf".to_string()),
        "doc" => Some("application/msword".to_string()),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()),
        "cbr" => Some("application/vnd.comicbook-rar".to_string()),
        "cbz" => Some("application/vnd.comicbook+zip".to_string()),
        _ => None,
    }
}

pub fn format_priority(ext: &str) -> u8 {
    match ext.to_lowercase().as_str() {
        "epub" => 0,
        "pdf" => 1,
        "mobi" => 2,
        "azw3" | "azw" => 3,
        "fb2" => 4,
        "djvu" => 5,
        "cbz" => 6,
        "cbr" => 7,
        "doc" | "docx" | "rtf" => 8,
        "txt" => 9,
        _ => 10,
    }
}

pub fn detect_isbn_query(value: &str) -> Option<String> {
    let compact: String = value
        .trim()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x')
        .collect();

    if compact.len() == 13 && compact.chars().all(|c| c.is_ascii_digit()) {
        Some(compact)
    } else if compact.len() == 10 {
        let last = compact.chars().last()?;
        let body = &compact[..9];
        if body.chars().all(|c| c.is_ascii_digit())
            && (last.is_ascii_digit() || last == 'X' || last == 'x')
        {
            Some(compact.to_uppercase())
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_search_url_title() {
        let url = build_search_url("storm front", None, &SearchColumn::TitleAuthor).unwrap();
        assert!(url.contains("req=storm+front"));
        assert!(url.contains("columns%5B%5D=t"));
        assert!(url.contains("columns%5B%5D=a"));
        assert!(url.contains("page=1"));
    }

    #[test]
    fn test_build_search_url_isbn() {
        let url = build_search_url("9780451457813", None, &SearchColumn::Isbn).unwrap();
        assert!(url.contains("columns%5B%5D=i"));
    }

    #[test]
    fn test_build_search_url_empty_returns_none() {
        assert!(build_search_url("", None, &SearchColumn::TitleAuthor).is_none());
        assert!(build_search_url("   ", None, &SearchColumn::TitleAuthor).is_none());
    }

    #[test]
    fn test_parse_size_to_bytes() {
        assert_eq!(parse_size_to_bytes("1 MB"), Some(1048576));
        assert_eq!(parse_size_to_bytes("1.5 MB"), Some(1572864));
        assert_eq!(parse_size_to_bytes("234 KB"), Some(239616));
        assert_eq!(parse_size_to_bytes("1 GB"), Some(1073741824));
        assert_eq!(parse_size_to_bytes(""), None);
    }

    #[test]
    fn test_extension_to_mime() {
        assert_eq!(
            extension_to_mime("epub"),
            Some("application/epub+zip".to_string())
        );
        assert_eq!(
            extension_to_mime("pdf"),
            Some("application/pdf".to_string())
        );
        assert_eq!(extension_to_mime("unknown"), None);
    }

    #[test]
    fn test_format_priority_ordering() {
        assert!(format_priority("epub") < format_priority("pdf"));
        assert!(format_priority("pdf") < format_priority("mobi"));
        assert!(format_priority("mobi") < format_priority("djvu"));
        assert!(format_priority("djvu") < format_priority("txt"));
    }

    #[test]
    fn test_detect_isbn_query() {
        assert_eq!(
            detect_isbn_query("9780451457813"),
            Some("9780451457813".to_string())
        );
        assert_eq!(
            detect_isbn_query("978-0-451-45781-3"),
            Some("9780451457813".to_string())
        );
        assert_eq!(
            detect_isbn_query("0-684-84328-5"),
            Some("0684843285".to_string())
        );
        assert_eq!(detect_isbn_query("storm front"), None);
        assert_eq!(detect_isbn_query(""), None);
    }

    #[test]
    fn test_parse_download_key() {
        let html = r#"<html><body>
            <a href="/get.php?md5=abc123&key=TESTKEY123">GET</a>
        </body></html>"#;
        assert_eq!(parse_download_key(html), Some("TESTKEY123".to_string()));
    }

    #[test]
    fn test_parse_download_key_missing() {
        let html = r#"<html><body><a href="/other.php">Link</a></body></html>"#;
        assert_eq!(parse_download_key(html), None);
    }

    #[test]
    fn test_build_download_url() {
        assert_eq!(
            build_download_url("abc123", "KEY456"),
            "https://libgen.bz/get.php?md5=abc123&key=KEY456"
        );
    }
}
