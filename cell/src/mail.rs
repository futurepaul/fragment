//! The platform's mail (docs/api.md, Mail): one plain-text message to one
//! person (`fragment_core::mail` checks what it holds), from the
//! deployment's address (`FRAGMENT_MAIL_FROM`), through Cloudflare Email
//! Sending's binding (`EMAIL`: wrangler.jsonc `send_email`). In dev and the
//! e2e it goes instead to the fake at `FRAGMENT_MAIL_URL` (crates/fakes,
//! mail.rs), which takes the binding's input as it is and sends nothing.
//! Transactional mail only: what a person asked for or is owed (an invite,
//! a seat), never a campaign.

use fragment_core::mail;
use fragment_proto::ErrorCode;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::js;

pub use fragment_core::mail::Mail;

/// What the fake answers, as the binding resolves (`{messageId}`) or
/// rejects (`{code, message}`).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    message_id: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

/// Sends `mail`: its message id.
pub async fn send(env: &Env, cfg: &Config, mail: &Mail) -> CellResult<String> {
    if let Some(why) = mail::refusal(mail) {
        return Err(CellError::invalid(why));
    }
    let from = cfg.mail_from.as_deref().ok_or_else(|| CellError::host("this deployment sends no mail: set its mail_from (FRAGMENT_MAIL_FROM)"))?;
    let message = mail::message(from, mail);
    let Some(url) = &cfg.mail_url else {
        return js::email_send(env.as_ref(), &message).await.map_err(|(code, why)| refused(&code, &why));
    };
    // the lower rung: the binding's input, to a fake at the vendor boundary
    let h = Headers::new();
    h.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(h).with_body(Some(message.to_string().into()));
    let mut resp = Fetch::Request(Request::new_with_init(&format!("{url}/send"), &init)?)
        .send()
        .await
        .map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("the mail fake did not answer: {e}")))?;
    let answer: Answer = resp.json().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("the mail fake's answer: {e}")))?;
    match (resp.status_code(), answer.message_id) {
        (200, Some(id)) => Ok(id),
        _ => Err(refused(answer.code.as_deref().unwrap_or(""), answer.message.as_deref().unwrap_or("no reason given"))),
    }
}

/// The service's refusal (its `E_…` code), as the platform answers it:
/// too many sent is the caller's to wait out, a message it will not take
/// (an address that bounced before, a field it refuses) is the caller's,
/// and the rest (an unverified sender, an outage) is the service failing.
fn refused(code: &str, why: &str) -> CellError {
    let error = match code {
        "E_RATE_LIMIT_EXCEEDED" => ErrorCode::RateLimited,
        "E_RECIPIENT_SUPPRESSED" | "E_VALIDATION_ERROR" | "E_FIELD_MISSING" | "E_TOO_MANY_RECIPIENTS" | "E_CONTENT_TOO_LARGE" => ErrorCode::InvalidRequest,
        _ => ErrorCode::UpstreamFailed,
    };
    CellError::new(error, format!("the mail was not sent ({}): {why}", if code.is_empty() { "no code" } else { code }))
}
