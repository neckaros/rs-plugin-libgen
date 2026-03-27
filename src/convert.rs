use rs_plugin_common_interfaces::{
    domain::{
        book::Book,
        person::Person,
        Relations,
    },
    lookup::{RsLookupMatchType, RsLookupMetadataResult, RsLookupMetadataResultWrapper},
    request::RsRequest,
    RsFileType,
};
use serde_json::json;

use crate::libgen::{extension_to_mime, LibgenBook};

fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut previous_was_dash = false;

    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            previous_was_dash = false;
        } else if !previous_was_dash && !slug.is_empty() {
            slug.push('-');
            previous_was_dash = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "unknown".to_string()
    } else {
        slug
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn language_to_code(language: &str) -> Option<String> {
    match language.trim().to_lowercase().as_str() {
        "english" => Some("en".to_string()),
        "french" | "français" => Some("fr".to_string()),
        "german" | "deutsch" => Some("de".to_string()),
        "spanish" | "español" => Some("es".to_string()),
        "italian" | "italiano" => Some("it".to_string()),
        "portuguese" | "português" => Some("pt".to_string()),
        "russian" | "русский" => Some("ru".to_string()),
        "chinese" | "中文" => Some("zh".to_string()),
        "japanese" | "日本語" => Some("ja".to_string()),
        "korean" | "한국어" => Some("ko".to_string()),
        "dutch" | "nederlands" => Some("nl".to_string()),
        "polish" | "polski" => Some("pl".to_string()),
        "arabic" | "العربية" => Some("ar".to_string()),
        "turkish" | "türkçe" => Some("tr".to_string()),
        "swedish" | "svenska" => Some("sv".to_string()),
        "czech" | "čeština" => Some("cs".to_string()),
        "romanian" | "română" => Some("ro".to_string()),
        "hungarian" | "magyar" => Some("hu".to_string()),
        "greek" | "ελληνικά" => Some("el".to_string()),
        "hindi" | "हिन्दी" => Some("hi".to_string()),
        other if other.len() == 2 || other.len() == 3 => Some(other.to_string()),
        _ => None,
    }
}

pub fn libgen_book_to_result(
    book: LibgenBook,
    match_type: Option<RsLookupMatchType>,
) -> RsLookupMetadataResultWrapper {
    let id = book
        .md5
        .as_ref()
        .map(|md5| format!("libgen:{md5}"))
        .unwrap_or_else(|| {
            book.file_id
                .as_ref()
                .map(|fid| format!("libgen-file:{fid}"))
                .unwrap_or_else(|| format!("libgen:{}", slugify(&book.title)))
        });

    let lang = language_to_code(&book.language);

    let mut params = serde_json::Map::new();
    if let Some(md5) = &book.md5 {
        params.insert("libgenMd5".to_string(), json!(md5));
    }
    if let Some(fid) = &book.file_id {
        params.insert("libgenFileId".to_string(), json!(fid));
    }
    if !book.extension.is_empty() {
        params.insert("extension".to_string(), json!(book.extension));
    }
    if !book.publisher.is_empty() {
        params.insert("publisher".to_string(), json!(book.publisher));
    }
    if let Some(size) = book.size_bytes {
        params.insert("fileSize".to_string(), json!(size));
    }

    let book_entity = Book {
        id,
        name: book.title.clone(),
        kind: Some("book".to_string()),
        year: book.year,
        pages: book.pages,
        lang,
        isbn13: book.isbn.clone().filter(|i| i.len() == 13),
        params: if params.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(params))
        },
        ..Default::default()
    };

    let people_details = if book.author.trim().is_empty() {
        None
    } else {
        let author_key = slugify(&book.author);
        let other_id = format!("libgen-author:{author_key}");
        let mut author_params = serde_json::Map::new();
        author_params.insert("otherids".to_string(), json!([other_id.clone()]));

        Some(vec![Person {
            id: other_id,
            name: book.author.clone(),
            kind: Some("author".to_string()),
            params: Some(serde_json::Value::Object(author_params)),
            generated: true,
            ..Default::default()
        }])
    };

    let relations = if people_details.is_some() {
        Some(Relations {
            people_details,
            ..Default::default()
        })
    } else {
        None
    };

    RsLookupMetadataResultWrapper {
        metadata: RsLookupMetadataResult::Book(book_entity),
        relations,
        match_type,
    }
}

pub fn libgen_book_to_request(book: &LibgenBook, download_url: String) -> RsRequest {
    let filename = if book.title.is_empty() {
        None
    } else {
        let sanitized = sanitize_filename(&book.title);
        if book.extension.is_empty() {
            Some(sanitized)
        } else {
            Some(format!("{sanitized}.{}", book.extension))
        }
    };

    let people_lookup = if book.author.trim().is_empty() {
        None
    } else {
        Some(vec![book.author.clone()])
    };

    RsRequest {
        url: download_url,
        mime: extension_to_mime(&book.extension),
        size: book.size_bytes,
        filename,
        permanent: false,
        instant: Some(true),
        kind: Some(RsFileType::Book),
        title: Some(book.title.clone()),
        language: language_to_code(&book.language),
        people_lookup,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Jim Butcher"), "jim-butcher");
        assert_eq!(slugify("J.R.R. Tolkien"), "j-r-r-tolkien");
        assert_eq!(slugify(""), "unknown");
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("A/B:C"), "A_B_C");
        assert_eq!(sanitize_filename("Normal Title"), "Normal Title");
    }

    #[test]
    fn test_language_to_code() {
        assert_eq!(language_to_code("English"), Some("en".to_string()));
        assert_eq!(language_to_code("french"), Some("fr".to_string()));
        assert_eq!(language_to_code("en"), Some("en".to_string()));
        assert_eq!(language_to_code("Unknown Language"), None);
    }

    #[test]
    fn test_libgen_book_to_result_sets_id() {
        let book = LibgenBook {
            md5: Some("abc123def456".to_string()),
            title: "Storm Front".to_string(),
            author: "Jim Butcher".to_string(),
            extension: "epub".to_string(),
            ..Default::default()
        };

        let result = libgen_book_to_result(book, None);
        if let RsLookupMetadataResult::Book(b) = &result.metadata {
            assert_eq!(b.id, "libgen:abc123def456");
            assert_eq!(b.name, "Storm Front");
        } else {
            panic!("Expected Book metadata");
        }
    }

    #[test]
    fn test_libgen_book_to_result_includes_author() {
        let book = LibgenBook {
            md5: Some("abc123".to_string()),
            title: "Test".to_string(),
            author: "Test Author".to_string(),
            ..Default::default()
        };

        let result = libgen_book_to_result(book, Some(RsLookupMatchType::ExactId));
        let relations = result.relations.expect("Expected relations");
        let people = relations.people_details.expect("Expected people");
        assert_eq!(people[0].name, "Test Author");
        assert_eq!(people[0].id, "libgen-author:test-author");
        assert_eq!(result.match_type, Some(RsLookupMatchType::ExactId));
    }

    #[test]
    fn test_libgen_book_to_request() {
        let book = LibgenBook {
            title: "Storm Front".to_string(),
            author: "Jim Butcher".to_string(),
            extension: "epub".to_string(),
            size_bytes: Some(349000),
            language: "English".to_string(),
            ..Default::default()
        };

        let req = libgen_book_to_request(&book, "https://libgen.bz/get.php?md5=abc&key=XYZ".to_string());
        assert_eq!(req.url, "https://libgen.bz/get.php?md5=abc&key=XYZ");
        assert_eq!(req.mime, Some("application/epub+zip".to_string()));
        assert_eq!(req.size, Some(349000));
        assert_eq!(req.filename, Some("Storm Front.epub".to_string()));
        assert!(!req.permanent);
        assert_eq!(req.instant, Some(true));
        assert_eq!(req.kind, Some(RsFileType::Book));
        assert_eq!(req.language, Some("en".to_string()));
    }
}
