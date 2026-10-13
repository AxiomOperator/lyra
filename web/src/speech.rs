//! Voice on your own servers (`[speech]`): speech to text with a Whisper
//! server and text to speech with Kokoro (or anything OpenAI-compatible:
//! `/audio/transcriptions`, `/audio/speech`). The app records and plays; this
//! passes the audio along for a paired device, so the speech servers stay
//! inside the network and are never called from a phone directly. Without
//! `[speech]` the app uses the browser's own recognition and voices.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Shared, device, error};

/// What `[speech]` set up (lyra fills it in from its config).
#[derive(Debug, Clone, Default)]
pub struct Speech {
    pub stt: Option<Stt>,
    pub tts: Option<Tts>,
    /// Hands-free: the app hears when you stop talking.
    pub vad: bool,
    /// Hands-free: talking over a reply being read stops it and listens.
    pub interruptions: bool,
    /// Replies are spoken a sentence at a time as they're ready.
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct Stt {
    /// The server's base URL, e.g. `http://172.16.0.14:8006/v1`.
    pub url: String,
    pub model: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Tts {
    pub url: String,
    pub model: String,
    pub voice: String,
    /// `mp3` (default), `opus`, `wav`…
    pub format: String,
    pub speed: Option<f32>,
}

/// Longest recording taken (about 10 minutes of speech).
pub const MAX_AUDIO: usize = 25 * 1024 * 1024;
/// Longest text spoken at once (the app sends a sentence or a paragraph).
const MAX_TEXT: usize = 4000;

/// `GET /api/speech`: what this server can do, for the app.
pub(crate) async fn info(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if let Err(r) = device(&s, &headers) {
        return *r;
    }
    let sp = s.speech.clone();
    // Only what answers and has its model: otherwise the app uses the browser's own.
    let (stt, tts) = (sp.stt.clone(), sp.tts.clone());
    let ready = tokio::task::spawn_blocking(move || (stt.is_some_and(|t| has_model(&t.url, &t.model)), tts.is_some_and(|t| has_model(&t.url, &t.model)))).await.unwrap_or((false, false));
    Json(json!({
        "stt": ready.0,
        "tts": sp.tts.as_ref().filter(|_| ready.1).map(|t| json!({ "voice": t.voice, "format": t.format })),
        "vad": sp.vad,
        "interruptions": sp.interruptions,
        "stream": sp.stream,
    }))
    .into_response()
}

/// The file name a recording goes up as (servers read its type from it).
fn file_name(content_type: &str) -> &'static str {
    let t = content_type.to_lowercase();
    if t.contains("mp4") || t.contains("m4a") || t.contains("aac") {
        "speech.m4a"
    } else if t.contains("ogg") {
        "speech.ogg"
    } else if t.contains("wav") {
        "speech.wav"
    } else if t.contains("mpeg") || t.contains("mp3") {
        "speech.mp3"
    } else {
        "speech.webm"
    }
}

/// `POST /api/speech/transcribe` (the recording as the body): `{ "text" }`.
pub(crate) async fn transcribe(State(s): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = device(&s, &headers) {
        return *r;
    }
    let Some(stt) = s.speech.stt.clone() else { return error(StatusCode::NOT_FOUND, "speech to text isn't set up on this server ([speech.stt])") };
    if body.is_empty() {
        return error(StatusCode::BAD_REQUEST, "no audio");
    }
    let kind = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("audio/webm").to_string();
    let done = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let part = reqwest::blocking::multipart::Part::bytes(body.to_vec()).file_name(file_name(&kind)).mime_str(kind.split(';').next().unwrap_or("audio/webm")).map_err(|e| e.to_string())?;
        let mut form = reqwest::blocking::multipart::Form::new().part("file", part).text("model", stt.model.clone()).text("response_format", "json");
        if let Some(l) = stt.language.clone().filter(|l| !l.is_empty()) {
            form = form.text("language", l);
        }
        let url = format!("{}/audio/transcriptions", stt.url.trim_end_matches('/'));
        let resp = crate::keys::auth(client(Duration::from_secs(120))?.post(&url), &url)
            .multipart(form)
            .send()
            .map_err(|e| format!("the speech-to-text server didn't answer: {e}"))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("speech to text failed ({status}): {}", text.chars().take(200).collect::<String>()));
        }
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok(v["text"].as_str().or(v.as_str()).unwrap_or("").trim().to_string())
    })
    .await;
    match done {
        Ok(Ok(text)) => Json(json!({ "text": text })).into_response(),
        Ok(Err(e)) => error(StatusCode::BAD_GATEWAY, &e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct Say {
    text: String,
    #[serde(default)]
    voice: Option<String>,
}

fn content_type(format: &str) -> &'static str {
    match format {
        "opus" | "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "pcm" => "audio/L16",
        _ => "audio/mpeg",
    }
}

/// `POST /api/speech/say` `{ text, voice? }`: the spoken text, as audio,
/// passed along as the speech server makes it.
pub(crate) async fn say(State(s): State<Arc<Shared>>, headers: HeaderMap, Json(b): Json<Say>) -> Response {
    if let Err(r) = device(&s, &headers) {
        return *r;
    }
    let Some(tts) = s.speech.tts.clone() else { return error(StatusCode::NOT_FOUND, "text to speech isn't set up on this server ([speech.tts])") };
    let text: String = b.text.trim().chars().take(MAX_TEXT).collect();
    if text.is_empty() {
        return error(StatusCode::BAD_REQUEST, "nothing to say");
    }
    let voice = b.voice.filter(|v| !v.trim().is_empty() && v.len() < 80).unwrap_or(tts.voice.clone());
    let mut req = json!({ "model": tts.model, "input": text, "voice": voice, "response_format": tts.format, "stream": s.speech.stream });
    if let Some(speed) = tts.speed {
        req["speed"] = json!(speed);
    }
    let kind = content_type(&tts.format);
    // The request on a blocking thread; its audio comes back in pieces as it's made.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    let (first_tx, first_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let url = format!("{}/audio/speech", tts.url.trim_end_matches('/'));
    tokio::task::spawn_blocking(move || {
        let resp = client(Duration::from_secs(300)).and_then(|c| crate::keys::auth(c.post(&url), &url).header(reqwest::header::CONTENT_TYPE, "application/json").body(req.to_string()).send().map_err(|e| format!("the text-to-speech server didn't answer: {e}")));
        let mut resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let status = r.status();
                let _ = first_tx.send(Err(format!("text to speech failed ({status}): {}", r.text().unwrap_or_default().chars().take(200).collect::<String>())));
                return;
            }
            Err(e) => {
                let _ = first_tx.send(Err(e));
                return;
            }
        };
        let _ = first_tx.send(Ok(()));
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match resp.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    if tx.blocking_send(Ok(Bytes::copy_from_slice(&buf[..n]))).is_err() {
                        return; // the app stopped listening
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }
    });
    match first_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return error(StatusCode::BAD_GATEWAY, &e),
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "the speech request ended"),
    }
    let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
    ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-store")], Body::from_stream(stream)).into_response()
}

/// `GET /api/speech/voices`: the voices the text-to-speech server has (ids).
pub(crate) async fn voices(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if let Err(r) = device(&s, &headers) {
        return *r;
    }
    let Some(tts) = s.speech.tts.clone() else { return error(StatusCode::NOT_FOUND, "text to speech isn't set up on this server ([speech.tts])") };
    let got = tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        let url = format!("{}/audio/voices", tts.url.trim_end_matches('/'));
        let v: Value = crate::keys::auth(client(Duration::from_secs(10))?.get(&url), &url).send().map_err(|e| e.to_string())?.text().map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))?;
        // Kokoro: {"voices": [{"id": …}]} or ["af_heart", …].
        let list = v.get("voices").cloned().unwrap_or(v);
        Ok(list.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string).or_else(|| x["id"].as_str().map(str::to_string))).collect())
    })
    .await;
    match got {
        Ok(Ok(v)) => Json(json!({ "voices": v, "voice": s.speech.tts.as_ref().map(|t| t.voice.clone()) })).into_response(),
        Ok(Err(e)) => error(StatusCode::BAD_GATEWAY, &e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// The server answers and has `model` (`GET /models`). One that doesn't list
/// any models (some don't) counts as having it.
fn has_model(url: &str, model: &str) -> bool {
    let Ok(c) = client(Duration::from_secs(4)) else { return false };
    let list = format!("{}/models", url.trim_end_matches('/'));
    let Ok(r) = crate::keys::auth(c.get(&list), &list).send() else { return false };
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return true;
    }
    let Ok(v) = r.text().map(|t| serde_json::from_str::<Value>(&t).unwrap_or(Value::Null)) else { return false };
    match v["data"].as_array() {
        Some(list) => list.iter().any(|m| m["id"].as_str() == Some(model)),
        None => true,
    }
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(timeout).build().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recordings_go_up_under_a_name_the_server_reads() {
        assert_eq!(file_name("audio/webm;codecs=opus"), "speech.webm");
        assert_eq!(file_name("audio/mp4"), "speech.m4a");
        assert_eq!(file_name("audio/ogg; codecs=opus"), "speech.ogg");
        assert_eq!(content_type("mp3"), "audio/mpeg");
        assert_eq!(content_type("opus"), "audio/ogg");
    }
}
