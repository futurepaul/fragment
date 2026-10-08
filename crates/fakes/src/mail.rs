//! Cloudflare Email Sending, faked over HTTP: in dev and the e2e the cell
//! POSTs its binding's input (`{to, from, subject, text}`) to `/send`
//! (`FRAGMENT_MAIL_URL`; cell/src/mail.rs), and this answers as the
//! binding does: `{messageId}`, or an error with the binding's `code`.
//! Every message sent is kept for a test to read (`sent`); `cargo xtask
//! dev` prints each, since dev never sends real mail. Lever: the next send
//! fails with a code of the binding's.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// A message as the platform sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub to: String,
    pub from: Value,
    pub subject: String,
    pub text: String,
}

#[derive(Default)]
struct State {
    sent: Vec<Sent>,
    /// The code the next send fails with.
    fail_next: Option<String>,
}

pub struct Mailer {
    pub url: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

/// The binding's refusal of a message it would not send.
fn refused(status: u16, code: &str, message: &str) -> Response {
    Response::json(status, &json!({ "code": code, "message": message }))
}

impl Mailer {
    /// On `port` (0: any); `print`: each message to stdout as it is sent.
    pub fn start(port: u16, print: bool) -> std::io::Result<Mailer> {
        let state: Arc<Mutex<State>> = Arc::default();
        let st = Arc::clone(&state);
        let handler: Handler = Arc::new(move |req: &Request| {
            if (req.method.as_str(), req.path.as_str()) != ("POST", "/send") {
                return Response::json(404, &json!({ "error": "not_found" }));
            }
            let mut s = st.lock().expect("mail state");
            if let Some(code) = s.fail_next.take() {
                return refused(500, &code, "a failure the test asked for");
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let field = |k: &str| body[k].as_str().unwrap_or("").to_string();
            let sent = Sent { to: field("to"), from: body["from"].clone(), subject: field("subject"), text: field("text") };
            // the binding's own checks, as far as the platform's mail reaches them
            let from_email = sent.from.as_str().or_else(|| sent.from["email"].as_str()).unwrap_or("");
            if sent.to.is_empty() || from_email.is_empty() || sent.subject.is_empty() || sent.text.is_empty() {
                return refused(400, "E_FIELD_MISSING", "to, from, subject and text are required");
            }
            if print {
                println!("mail to {} from {}: {}\n{}\n", sent.to, sent.from, sent.subject, sent.text);
            }
            s.sent.push(sent);
            Response::json(200, &json!({ "messageId": format!("fake-{}", s.sent.len()) }))
        });
        let server = Server::start(port, handler)?;
        Ok(Mailer { url: server.url.clone(), state, _server: server })
    }

    /// Every message sent, oldest first.
    pub fn sent(&self) -> Vec<Sent> {
        self.state.lock().expect("mail state").sent.clone()
    }

    /// The messages sent to `to`, oldest first.
    pub fn sent_to(&self, to: &str) -> Vec<Sent> {
        self.sent().into_iter().filter(|m| m.to == to).collect()
    }

    /// The next send fails with the binding's `code` (`E_RATE_LIMIT_EXCEEDED`, …).
    pub fn fail_next(&self, code: &str) {
        self.state.lock().expect("mail state").fail_next = Some(code.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: the fake answers as the binding does: a message id for a
    /// whole message, kept for the test; a missing field refused; a lever's
    /// code once. Method: posts to it as the cell does.
    #[test]
    fn it_keeps_what_it_is_sent() {
        let mailer = Mailer::start(0, false).unwrap();
        let send = |body: Value| crate::http::post(&format!("{}/send", mailer.url), &[("content-type", "application/json")], body.to_string().as_bytes()).unwrap();
        let from = json!({ "email": "mail@fragment.localhost", "name": "Fragment" });
        let whole = json!({ "to": "bob@example.com", "from": from, "subject": "s", "text": "t" });
        assert_eq!(send(whole.clone()), 200);
        assert_eq!(mailer.sent_to("bob@example.com"), vec![Sent { to: "bob@example.com".into(), from, subject: "s".into(), text: "t".into() }]);
        let bare = json!({ "to": "bob@example.com", "from": "mail@fragment.localhost", "subject": "s", "text": "t" });
        assert_eq!(send(bare.clone()), 200);
        assert_eq!(mailer.sent()[1].from, bare["from"]);
        assert_eq!(send(json!({ "to": "bob@example.com", "from": { "name": "Fragment" }, "subject": "s", "text": "t" })), 400);
        assert_eq!(send(json!({ "to": "bob@example.com", "from": "f", "subject": "s" })), 400);
        mailer.fail_next("E_RATE_LIMIT_EXCEEDED");
        assert_eq!(send(whole.clone()), 500);
        assert_eq!(send(whole), 200);
        assert_eq!(mailer.sent().len(), 3, "a refused message is not kept");
    }
}
