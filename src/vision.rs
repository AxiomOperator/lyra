//! Reading what isn't text: PDFs (their text layer, else their scanned
//! pages) and images, for the project folders, OneDrive files and attachments.
//! Pictures go to the vision model (`[vision]`, any OpenAI-compatible model
//! that takes `image_url` parts, e.g. Qwen-VL on llama-server with --mmproj),
//! which answers in text, so the chat model needn't see.

use std::sync::RwLock;
use std::time::Duration;

use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

/// `[vision]`.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    /// Its base URL, e.g. `http://gpu:8090/v1` (`/chat/completions` is added).
    pub url: String,
    pub model: String,
    /// Most scanned pages looked at in one read.
    #[serde(default = "default_pages")]
    pub max_pages: usize,
}

fn default_pages() -> usize {
    6
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(settings: Option<Settings>) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = settings;
}

fn settings() -> Option<Settings> {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

const IMAGES: &[(&str, &str)] = &[("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp"), ("bmp", "image/bmp")];

/// The image type of a file name, if it's one the vision model takes.
pub fn image_type(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    IMAGES.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

/// A file lyra reads with this module (a PDF or an image).
pub fn handles(name: &str) -> bool {
    name.to_lowercase().ends_with(".pdf") || image_type(name).is_some()
}

const LOOK: &str = "Transcribe all the text you can see, exactly, keeping its layout where it matters (tables as Markdown). \
Then describe briefly anything else that matters: photos, diagrams, stamps, signatures, handwriting. Don't guess at what you can't read.";

/// Ask the vision model about images (`(mime, bytes)`).
pub fn look(images: &[(&str, &[u8])], question: Option<&str>) -> Result<String, String> {
    let s = settings().ok_or("lyra has no vision model to look at pictures and scans: add one under [vision] in config.toml")?;
    let mut parts: Vec<Value> = vec![json!({ "type": "text", "text": question.filter(|q| !q.trim().is_empty()).map_or(LOOK.to_string(), |q| format!("{q}\n\n{LOOK}")) })];
    parts.extend(images.iter().map(|(mime, bytes)| json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)) } })));
    let body = json!({ "model": s.model, "messages": [{ "role": "user", "content": parts }], "temperature": 0.1, "max_tokens": 4096 });
    let started = std::time::Instant::now();
    let resp = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?
        .post(format!("{}/chat/completions", s.url.trim_end_matches('/')))
        .json(&body)
        .send()
        .map_err(|e| format!("the vision model: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("the vision model: {status}: {}", resp.text().unwrap_or_default().chars().take(300).collect::<String>()));
    }
    let reply: Value = resp.json().map_err(|e| e.to_string())?;
    crate::usage::record_usage("vision", &s.model, &reply["usage"], started.elapsed().as_millis() as u64);
    let text = reply["choices"][0]["message"]["content"].as_str().unwrap_or("").trim();
    // Thinking models: only what comes after their thoughts.
    let text = text.rsplit_once("</think>").map_or(text, |(_, t)| t).trim();
    if text.is_empty() {
        return Err("the vision model gave no answer".into());
    }
    Ok(text.to_string())
}

/// A PDF's text layer, page by page.
fn pdf_pages(bytes: &[u8]) -> Result<Vec<String>, String> {
    // pdf-extract panics on some odd files; a bad PDF mustn't take lyra down.
    std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes)).map_err(|_| "this PDF couldn't be read".to_string())?.map_err(|e| format!("this PDF couldn't be read: {e}"))
}

/// The scanned pages' pictures (the JPEGs scanners put in), first `max` pages.
fn pdf_scans(bytes: &[u8], max: usize) -> Result<(Vec<Vec<u8>>, usize), String> {
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| format!("this PDF couldn't be opened: {e}"))?;
    let pages = doc.get_pages();
    let mut out = Vec::new();
    for (_, id) in pages.iter().take(max) {
        for img in doc.get_page_images(*id).unwrap_or_default() {
            if img.filters.as_ref().is_some_and(|f| f.len() == 1 && f[0] == "DCTDecode") && img.width >= 200 && img.height >= 200 {
                out.push(img.content.to_vec());
            }
        }
    }
    Ok((out, pages.len()))
}

/// Mostly no text: a scan (a few stray characters per page don't count).
fn scanned(pages: &[String]) -> bool {
    let chars: usize = pages.iter().map(|p| p.chars().filter(|c| c.is_alphanumeric()).count()).sum();
    chars < 40 * pages.len().max(1)
}

/// What's in a PDF: its text, or what the vision model reads off its scanned pages.
pub fn pdf(bytes: &[u8], question: Option<&str>) -> Result<String, String> {
    if let Some(text) = pdf_text(bytes) {
        return Ok(text);
    }
    let max = settings().map_or(default_pages(), |s| s.max_pages.clamp(1, 20));
    let (scans, total) = pdf_scans(bytes, max)?;
    if scans.is_empty() {
        return Err("this PDF has no text and no scanned pages lyra can pick out".into());
    }
    if settings().is_none() {
        return Err(format!("this PDF is a scan ({total} page{}): lyra needs a vision model to read it (add [vision] to config.toml)", if total == 1 { "" } else { "s" }));
    }
    let imgs: Vec<(&str, &[u8])> = scans.iter().map(|b| ("image/jpeg", b.as_slice())).collect();
    let text = look(&imgs, question)?;
    Ok(if total > max { format!("{text}\n\n(read the first {max} of {total} scanned pages)") } else { text })
}

/// A PDF's text when it has a text layer (quick: no model), else `None`.
pub fn pdf_text(bytes: &[u8]) -> Option<String> {
    let pages = pdf_pages(bytes).ok()?;
    (!pages.is_empty() && !scanned(&pages)).then(|| pages.iter().enumerate().map(|(i, p)| format!("[page {}]\n{}", i + 1, p.trim().replace('\t', " "))).collect::<Vec<_>>().join("\n\n"))
}

/// What a PDF or an image file shows, as text.
pub fn read(name: &str, bytes: &[u8], question: Option<&str>) -> Result<String, String> {
    if name.to_lowercase().ends_with(".pdf") {
        return pdf(bytes, question);
    }
    match image_type(name) {
        Some(mime) => look(&[(mime, bytes)], question),
        None => Err(format!("{name}: not a PDF or a picture")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_are_told_from_text_pdfs_and_images_by_name() {
        assert!(scanned(&["".into(), " 1 ".into()]));
        assert!(!scanned(&["The firewall rules for the new building, phase one and two.".into()]));
        assert_eq!(image_type("Photo.JPG"), Some("image/jpeg"));
        assert!(handles("scan.pdf") && handles("a.webp") && !handles("notes.md"));
    }

    #[test]
    fn without_a_vision_model_pictures_say_what_to_add() {
        configure(None);
        assert!(look(&[("image/png", b"x")], None).unwrap_err().contains("[vision]"));
        assert!(pdf(b"not a pdf", None).is_err());
    }
}

#[cfg(test)]
mod try_files {
    /// `LYRA_TRY_PDF=file.pdf cargo test try_a_pdf -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn try_a_pdf() {
        let bytes = std::fs::read(std::env::var("LYRA_TRY_PDF").unwrap()).unwrap();
        let text = super::pdf_text(&bytes);
        let (scans, pages) = super::pdf_scans(&bytes, 6).unwrap();
        println!("pages {pages}, scans {}, text: {:?}", scans.len(), text.map(|t| t.chars().take(300).collect::<String>()));
    }
}
