use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chardetng::EncodingDetector;
use encoding_rs::Encoding;
use regex::Regex;
use scraper::{Html, Node};
use serde::{Deserialize, Serialize};

const PARSER_VERSION: u32 = 2;
const MAX_TEXT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Book {
    pub path: PathBuf,
    pub title: String,
    pub chapters: Vec<Chapter>,
    pub detected_encoding: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ParsedCache {
    version: u32,
    source_len: u64,
    modified_nanos: u128,
    edge_hash: u64,
    chapters: Vec<Chapter>,
    detected_encoding: Option<String>,
}

pub fn open(path: &Path, encoding: Option<&str>) -> Result<Book> {
    let path = path
        .canonicalize()
        .with_context(|| format!("无法打开 {}", path.display()))?;
    let title = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let metadata = fs::metadata(&path)?;
    let modified_nanos = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let edge_hash = file_edge_hash(&path)?;
    let cache_path = cache_path(&path, encoding);
    if let Some(cache) = cache_path
        .as_ref()
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<ParsedCache>(&b).ok())
    {
        if cache.version == PARSER_VERSION
            && cache.source_len == metadata.len()
            && cache.modified_nanos == modified_nanos
            && cache.edge_hash == edge_hash
        {
            return Ok(Book {
                path,
                title,
                chapters: cache.chapters,
                detected_encoding: cache.detected_encoding,
            });
        }
    }
    let (chapters, detected_encoding) = match ext.as_str() {
        "txt" => {
            let bytes = fs::read(&path)?;
            if bytes.len() > 128 * 1024 * 1024 {
                bail!("TXT 文件超过 128 MiB 限制");
            }
            let (text, label) = decode_txt(&bytes, encoding)?;
            (split_txt(&text)?, Some(label))
        }
        "epub" => (read_epub(&path)?, None),
        "mobi" => (read_mobi(&path)?, None),
        _ => bail!("暂不支持 .{ext} 格式；支持 txt、epub、mobi"),
    };
    if chapters.is_empty() {
        bail!("未找到可阅读的文字");
    }
    if chapters.iter().map(|c| c.text.len()).sum::<usize>() > MAX_TEXT_BYTES {
        bail!("提取后的正文超过 128 MiB 限制");
    }
    if let Some(cache_path) = cache_path {
        let cache = ParsedCache {
            version: PARSER_VERSION,
            source_len: metadata.len(),
            modified_nanos,
            edge_hash,
            chapters: chapters.clone(),
            detected_encoding: detected_encoding.clone(),
        };
        let _ = write_cache(&cache_path, &cache);
    }
    Ok(Book {
        path,
        title,
        chapters,
        detected_encoding,
    })
}

fn cache_path(path: &Path, encoding: Option<&str>) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("com", "xmread", "xmread")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    encoding.unwrap_or("auto").hash(&mut hasher);
    let name = format!("{:016x}.json", hasher.finish());
    Some(dirs.cache_dir().join(name))
}

fn file_edge_hash(path: &Path) -> Result<u64> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut buf = [0_u8; 4096];
    let n = file.read(&mut buf)?;
    buf[..n].hash(&mut hasher);
    if len > 4096 {
        file.seek(SeekFrom::End(-4096))?;
        let n = file.read(&mut buf)?;
        buf[..n].hash(&mut hasher);
    }
    Ok(hasher.finish())
}

fn write_cache(path: &Path, cache: &ParsedCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec(cache)?)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

fn decode_txt(bytes: &[u8], override_label: Option<&str>) -> Result<(String, String)> {
    let encoding = if let Some(label) = override_label {
        Encoding::for_label(label.as_bytes()).with_context(|| format!("未知编码：{label}"))?
    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        encoding_rs::UTF_8
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        encoding_rs::UTF_16LE
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        encoding_rs::UTF_16BE
    } else if std::str::from_utf8(bytes).is_ok() {
        encoding_rs::UTF_8
    } else {
        let mut detector = EncodingDetector::new();
        detector.feed(bytes, true);
        detector.guess(None, true)
    };
    let (text, _, had_errors) = encoding.decode(bytes);
    if had_errors && override_label.is_some() {
        bail!("使用 {} 解码时遇到无效字节", encoding.name());
    }
    Ok((
        text.trim_start_matches('\u{feff}').to_string(),
        encoding.name().to_string(),
    ))
}

fn split_txt(text: &str) -> Result<Vec<Chapter>> {
    let pattern = Regex::new(
        r"(?i)^\s*(?:第[零〇一二三四五六七八九十百千万两\d]+章(?:[\s:：、.．-].*)?|chapter\s+\d+(?:[\s:：、.．-].*)?)\s*$",
    )?;
    let mut headings = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let title = line.trim();
        if title.chars().count() <= 80 && pattern.is_match(title) && !title.contains("……") {
            headings.push((offset, title.to_string()));
        }
        offset += line.len();
    }
    if headings.len() < 2 {
        return Ok(vec![Chapter {
            title: "全文".into(),
            text: normalize_text(text),
        }]);
    }
    let mut chapters = Vec::new();
    if headings[0].0 > 0 {
        let intro = normalize_text(&text[..headings[0].0]);
        if !intro.is_empty() {
            chapters.push(Chapter {
                title: "前言".into(),
                text: intro,
            });
        }
    }
    for (i, (start, title)) in headings.iter().enumerate() {
        let end = headings.get(i + 1).map_or(text.len(), |next| next.0);
        let body = normalize_text(&text[*start..end]);
        if !body.is_empty() {
            chapters.push(Chapter {
                title: title.clone(),
                text: body,
            });
        }
    }
    Ok(chapters)
}

fn read_epub(path: &Path) -> Result<Vec<Chapter>> {
    let file = fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file).context("EPUB 压缩包无效")?;
    let container = zip_text(&mut zip, "META-INF/container.xml")?;
    let xml = roxmltree::Document::parse(&container)?;
    let opf_path = xml
        .descendants()
        .find(|n| n.has_tag_name("rootfile"))
        .and_then(|n| n.attribute("full-path"))
        .context("EPUB 缺少 OPF 路径")?
        .to_string();
    let opf = zip_text(&mut zip, &opf_path)?;
    let package = roxmltree::Document::parse(&opf)?;
    let base = opf_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut items = std::collections::HashMap::new();
    for item in package.descendants().filter(|n| n.has_tag_name("item")) {
        if let (Some(id), Some(href)) = (item.attribute("id"), item.attribute("href")) {
            let props = item.attribute("properties").unwrap_or("");
            let media = item.attribute("media-type").unwrap_or("");
            items.insert(
                id.to_string(),
                (href.to_string(), props.to_string(), media.to_string()),
            );
        }
    }
    let mut chapters = Vec::new();
    for itemref in package.descendants().filter(|n| n.has_tag_name("itemref")) {
        if itemref.attribute("linear") == Some("no") {
            continue;
        }
        let Some(id) = itemref.attribute("idref") else {
            continue;
        };
        let Some((href, props, media)) = items.get(id) else {
            continue;
        };
        if props
            .split_whitespace()
            .any(|p| p == "nav" || p == "cover-image")
        {
            continue;
        }
        if media != "application/xhtml+xml" && media != "text/html" {
            continue;
        }
        let resolved = normalize_zip_path(base, href)?;
        let html = zip_text(&mut zip, &resolved)?;
        if !has_prose_content(&html) {
            continue;
        }
        let text = html_to_text(&html);
        if text.trim().is_empty() {
            continue;
        }
        let title = first_heading(&html).unwrap_or_else(|| format!("章节 {}", chapters.len() + 1));
        if (title.to_ascii_lowercase().contains("table of contents") || title.trim() == "目录")
            && text.chars().count() < 1000
        {
            continue;
        }
        let split = split_txt(&text)?;
        if split.len() > 1 {
            chapters.extend(split);
        } else {
            chapters.push(Chapter { title, text });
        }
    }
    Ok(chapters)
}

fn zip_text(zip: &mut zip::ZipArchive<fs::File>, name: &str) -> Result<String> {
    let mut entry = zip
        .by_name(name)
        .with_context(|| format!("EPUB 缺少 {name}"))?;
    if entry.size() > 16 * 1024 * 1024 {
        bail!("EPUB 条目过大：{name}");
    }
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn normalize_zip_path(base: &str, href: &str) -> Result<String> {
    let raw = format!("{base}/{href}").replace('\\', "/");
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("无效的 EPUB 路径");
                }
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

fn read_mobi(path: &Path) -> Result<Vec<Chapter>> {
    let mobi =
        mobi::Mobi::from_path(path).context("MOBI 无法解析，可能是加密文件或不兼容的变体")?;
    if mobi.encryption() != mobi::headers::Encryption::No {
        bail!("不支持 DRM 加密的 MOBI");
    }
    let html = mobi.content_as_string().context("MOBI 正文无法提取")?;
    let text = html_to_text(&html);
    split_txt(&text)
}

fn first_heading(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let selector = scraper::Selector::parse("h1,h2,h3,title").ok()?;
    doc.select(&selector)
        .map(|e| e.text().collect::<String>().trim().to_string())
        .find(|s| !s.is_empty())
}

fn html_to_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    let mut out = String::new();
    for node in doc.root_element().descendants() {
        if node.ancestors().any(|a| match a.value() {
            Node::Element(e) => matches!(e.name(), "head" | "script" | "style" | "svg" | "nav"),
            _ => false,
        }) {
            continue;
        }
        match node.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(e)
                if matches!(
                    e.name(),
                    "p" | "div"
                        | "section"
                        | "article"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "li"
                        | "br"
                        | "blockquote"
                ) =>
            {
                out.push('\n')
            }
            _ => {}
        }
    }
    normalize_text(&out)
}

fn has_prose_content(html: &str) -> bool {
    let doc = Html::parse_document(html);
    doc.root_element().descendants().any(|node| {
        matches!(node.value(), Node::Text(text) if !text.trim().is_empty())
            && !node.ancestors().any(|a| match a.value() {
                Node::Element(e) => matches!(
                    e.name(),
                    "head"
                        | "script"
                        | "style"
                        | "svg"
                        | "nav"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                ),
                _ => false,
            })
    })
}

fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_chapters_and_fallback() {
        let chapters = split_txt("第1章 开始\n甲\n第2章 继续\n乙").unwrap();
        assert_eq!(chapters.len(), 2);
        assert!(chapters[1].text.contains("乙"));
        assert_eq!(split_txt("没有章节\n正文").unwrap().len(), 1);
    }

    #[test]
    fn html_omits_images_and_scripts() {
        let text = html_to_text(
            "<html><head><title>重复标题</title></head><body><h1>第一章</h1><p>你好<img src='x'>世界</p><script>secret</script></body></html>",
        );
        assert!(text.contains("你好世界"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("src"));
        assert!(!text.contains("重复标题"));
        assert!(!has_prose_content(
            "<html><head><title>第一卷</title></head><body><h1>正文</h1></body></html>"
        ));
    }

    #[test]
    fn epub_uses_spine_order_and_skips_navigation() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "xmread-{}-{}.epub",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let file = fs::File::create(&path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("META-INF/container.xml", options).unwrap();
            zip.write_all(br#"<container><rootfiles><rootfile full-path="OPS/content.opf"/></rootfiles></container>"#).unwrap();
            zip.start_file("OPS/content.opf", options).unwrap();
            zip.write_all(br#"<package><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/><item id="section" href="section.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="section"/><itemref idref="b"/><itemref idref="nav"/><itemref idref="a"/></spine></package>"#).unwrap();
            zip.start_file("OPS/section.xhtml", options).unwrap();
            zip.write_all(
                b"<html><head><title>Volume</title></head><body><h1>Body</h1></body></html>",
            )
            .unwrap();
            zip.start_file("OPS/a.xhtml", options).unwrap();
            zip.write_all(b"<h1>A</h1><p>last</p>").unwrap();
            zip.start_file("OPS/b.xhtml", options).unwrap();
            zip.write_all(b"<h1>B</h1><p>first</p><img src='cover.jpg'>")
                .unwrap();
            zip.start_file("OPS/nav.xhtml", options).unwrap();
            zip.write_all(b"<h1>Navigation</h1>").unwrap();
            zip.finish().unwrap();
        }
        let chapters = read_epub(&path).unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "B");
        assert_eq!(chapters[1].title, "A");
        assert!(!chapters[0].text.contains("cover.jpg"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parsed_cache_tracks_source_changes() {
        let path = std::env::temp_dir().join(format!(
            "xmread-cache-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "第一版").unwrap();
        let first = open(&path, None).unwrap();
        assert_eq!(first.chapters[0].text, "第一版");
        fs::write(&path, "第二版本").unwrap();
        let second = open(&path, None).unwrap();
        assert_eq!(second.chapters[0].text, "第二版本");
        let _ = fs::remove_file(path);
    }
}
