//! WHAT: how mail actually leaves - the transport trait and its two backends.
//! WHY:  local development must never send a real email to a real person, so
//!       the default transport is Mailpit in Docker, which accepts everything
//!       and shows it in a web UI. Production swaps in an HTTP provider without
//!       any caller changing.
//! HOW:  `EmailSender` is one method. SmtpSender speaks SMTP through lettre;
//!       HttpSender posts to Postmark. A third implementation, used by the
//!       tests, records what it was asked to send - and one that always fails,
//!       to exercise the retry path.

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message as LettreMessage, Tokio1Executor};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmailAttachment {
    pub filename: String,
    pub content_type: String,
    /// base64 so the whole email survives a round trip through outbox.payload
    pub content_base64: String,
}

impl EmailAttachment {
    pub fn from_bytes(filename: &str, content_type: &str, bytes: &[u8]) -> Self {
        Self {
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(base64::engine::general_purpose::STANDARD.decode(&self.content_base64)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutgoingEmail {
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    pub from: String,
    pub subject: String,
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub attachments: Vec<EmailAttachment>,
    /// Set on lead emails so the worker can flip the lead to 'sent' once the
    /// message really left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_id: Option<String>,
}

#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(&self, email: &OutgoingEmail) -> Result<()>;
    fn describe(&self) -> String;
}

pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    url: String,
}

impl SmtpSender {
    /// `url` is a lettre connection string, e.g. smtp://localhost:1025 for
    /// Mailpit (no TLS, no auth - it is a local sink).
    pub fn new(url: &str) -> Result<Self> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .with_context(|| format!("bad SMTP_URL: {url}"))?
            .build();
        Ok(Self {
            transport,
            url: url.to_string(),
        })
    }
}

fn build_message(email: &OutgoingEmail) -> Result<LettreMessage> {
    let mut builder = LettreMessage::builder()
        .from(email.from.parse().context("bad From address")?)
        .subject(&email.subject);
    for to in &email.to {
        builder = builder.to(to.parse().with_context(|| format!("bad To address: {to}"))?);
    }
    for cc in &email.cc {
        builder = builder.cc(cc.parse().with_context(|| format!("bad Cc address: {cc}"))?);
    }

    let body = match &email.html {
        Some(html) => MultiPart::alternative_plain_html(email.text.clone(), html.clone()),
        None => MultiPart::mixed().singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(email.text.clone()),
        ),
    };

    let mut mixed = MultiPart::mixed().multipart(body);
    for att in &email.attachments {
        mixed = mixed.singlepart(
            Attachment::new(att.filename.clone()).body(
                att.bytes()?,
                att.content_type
                    .parse()
                    .unwrap_or(ContentType::TEXT_PLAIN),
            ),
        );
    }
    Ok(builder.multipart(mixed)?)
}

#[async_trait]
impl EmailSender for SmtpSender {
    async fn send(&self, email: &OutgoingEmail) -> Result<()> {
        self.transport
            .send(build_message(email)?)
            .await
            .context("SMTP send failed")?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("smtp({})", self.url)
    }
}

/// Postmark. Selected with EMAIL_TRANSPORT=http.
pub struct HttpSender {
    http: reqwest::Client,
    token: String,
    endpoint: String,
}

impl HttpSender {
    pub fn postmark(token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            endpoint: "https://api.postmarkapp.com/email".to_string(),
        }
    }
}

#[async_trait]
impl EmailSender for HttpSender {
    async fn send(&self, email: &OutgoingEmail) -> Result<()> {
        let body = serde_json::json!({
            "From": email.from,
            "To": email.to.join(","),
            "Cc": email.cc.join(","),
            "Subject": email.subject,
            "TextBody": email.text,
            "HtmlBody": email.html,
            "Attachments": email.attachments.iter().map(|a| serde_json::json!({
                "Name": a.filename,
                "Content": a.content_base64,
                "ContentType": a.content_type,
            })).collect::<Vec<_>>(),
        });
        let res = self
            .http
            .post(&self.endpoint)
            .header("X-Postmark-Server-Token", &self.token)
            .json(&body)
            .send()
            .await
            .context("Postmark request failed")?;
        let status = res.status();
        if !status.is_success() {
            let detail = res.text().await.unwrap_or_default();
            anyhow::bail!("Postmark returned {status}: {detail}");
        }
        Ok(())
    }

    fn describe(&self) -> String {
        "http(postmark)".to_string()
    }
}

/// Keeps every message in memory. Tests assert against this; it is also what
/// runs when no transport is configured, so a misconfigured demo logs instead of
/// silently dropping mail.
#[derive(Default)]
pub struct CapturingSender {
    pub sent: Mutex<Vec<OutgoingEmail>>,
}

#[async_trait]
impl EmailSender for CapturingSender {
    async fn send(&self, email: &OutgoingEmail) -> Result<()> {
        tracing::info!(to = ?email.to, subject = %email.subject, "email captured (no transport configured)");
        self.sent.lock().unwrap().push(email.clone());
        Ok(())
    }

    fn describe(&self) -> String {
        "capture(in-memory)".to_string()
    }
}

/// Always fails. Exists so the retry-and-give-up path has a test.
pub struct FailingSender;

#[async_trait]
impl EmailSender for FailingSender {
    async fn send(&self, _email: &OutgoingEmail) -> Result<()> {
        anyhow::bail!("connection refused")
    }

    fn describe(&self) -> String {
        "failing(test)".to_string()
    }
}

/// EMAIL_TRANSPORT=smtp|http, defaulting to the in-memory capture so a fresh
/// checkout runs without a mail server.
pub fn from_env() -> Arc<dyn EmailSender> {
    crate::load_env();
    match std::env::var("EMAIL_TRANSPORT").unwrap_or_default().as_str() {
        "smtp" => {
            let url = std::env::var("SMTP_URL").unwrap_or_else(|_| "smtp://localhost:1025".into());
            match SmtpSender::new(&url) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::error!("SMTP unavailable ({e}); capturing mail instead");
                    Arc::new(CapturingSender::default())
                }
            }
        }
        "http" => match std::env::var("POSTMARK_TOKEN") {
            Ok(t) if !t.trim().is_empty() => Arc::new(HttpSender::postmark(t)),
            _ => {
                tracing::error!("POSTMARK_TOKEN missing; capturing mail instead");
                Arc::new(CapturingSender::default())
            }
        },
        _ => Arc::new(CapturingSender::default()),
    }
}

pub fn from_address() -> String {
    crate::load_env();
    std::env::var("EMAIL_FROM").unwrap_or_else(|_| "Automotrix Bot <bot@automotrix-demo.com>".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OutgoingEmail {
        OutgoingEmail {
            to: vec!["leads@automotrix-demo.com".into()],
            cc: vec![],
            from: "bot@automotrix-demo.com".into(),
            subject: "New lead: María Ñúñez, 2021 Toyota RAV4".into(),
            text: "body".into(),
            html: Some("<p>body</p>".into()),
            attachments: vec![EmailAttachment::from_bytes(
                "lead.xml",
                "application/xml",
                b"<?xml version=\"1.0\"?><adf/>",
            )],
            lead_id: None,
        }
    }

    #[test]
    fn builds_a_mime_message_with_attachment() {
        let msg = build_message(&sample()).unwrap();
        let raw = String::from_utf8_lossy(&msg.formatted()).to_string();
        assert!(raw.contains("lead.xml"));
        assert!(raw.contains("multipart/"));
    }

    #[test]
    fn attachments_survive_a_json_round_trip() {
        let e = sample();
        let back: OutgoingEmail = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        assert_eq!(back.attachments[0].bytes().unwrap(), b"<?xml version=\"1.0\"?><adf/>");
    }
}
