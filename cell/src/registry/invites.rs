//! Invites by email (docs/cloudflare-v1.md, decision 48). A fragment shares
//! with an email: the person who holds it is a member at once. When no one
//! does yet, the invite waits on the email in the fragment (members.rs),
//! and here, kept against the email, which the platform mails a link to
//! the fragment. A sign-in that verifies the email meets every invite
//! waiting on it: each fragment makes its person a member
//! (`invites/claim`). Signing in never makes a share; it meets one
//! addressed to its email. The mails a person has sent are capped by the
//! day.

use fragment_core::mail;
use fragment_proto::{limits, ErrorCode, Role};
use futures_util::future::join_all;
use serde::Deserialize;
use worker::*;

use super::calls::{ClaimInvite, InviteEmail, Invited, Lookup};
use super::RegistryCell;
use crate::error::{CellError, CellResult};
use crate::js;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS invites_waiting (
  email TEXT NOT NULL, fragment TEXT NOT NULL, expires_at INTEGER NOT NULL, PRIMARY KEY (email, fragment));
CREATE INDEX IF NOT EXISTS invites_waiting_expiry ON invites_waiting (expires_at);
CREATE TABLE IF NOT EXISTS invite_mails (
  sharer TEXT NOT NULL, day INTEGER NOT NULL, mailed INTEGER NOT NULL, PRIMARY KEY (sharer, day));
";

const DAY_MS: i64 = 24 * 3600 * 1000;

/// Fragments a sign-in tells at once that it met their invites.
const CLAIMS_AT_ONCE: usize = 32;

#[derive(Deserialize)]
struct FragmentRow {
    fragment: String,
}

impl RegistryCell {
    /// `/invite`: the person who holds the email, or the invite kept
    /// against it and mailed. The row is written before the mail goes, so a
    /// sign-in meanwhile meets it, and taken back when the mail is not sent.
    pub(super) async fn invite(&self, i: InviteEmail) -> CellResult<Invited> {
        if !mail::valid_address(&i.email) || i.email != i.email.to_ascii_lowercase() {
            return Err(CellError::invalid(format!("{:?} is not a lower-case email", i.email)));
        }
        match self.lookup(Lookup { who: i.email.clone() }) {
            Ok(person) => return Ok(Invited::Holder(person)),
            Err(e) if e.code == ErrorCode::NotFound => {}
            Err(e) => return Err(e),
        }
        let now = js::now_ms();
        if i.expires_at <= now {
            return Err(CellError::invalid("an invite expires later than now"));
        }
        let from = self.email_of(&i.sharer)?.ok_or_else(|| CellError::new(ErrorCode::Forbidden, "only a person who signs in shares by email"))?;
        self.exec("DELETE FROM invites_waiting WHERE expires_at <= ?", vec![SqlStorageValue::Integer(now)])?;
        let waiting = self.count(
            "SELECT COUNT(*) AS n FROM invites_waiting WHERE email = ? AND fragment != ?",
            vec![i.email.as_str().into(), i.fragment.as_str().into()],
        )?;
        if waiting >= limits::INVITES_PER_EMAIL_MAX as u64 {
            return Err(CellError::new(ErrorCode::RateLimited, format!("{} invites wait on {} already", limits::INVITES_PER_EMAIL_MAX, i.email)));
        }
        self.count_mail(&i.sharer, now)?;
        self.exec(
            "INSERT INTO invites_waiting (email, fragment, expires_at) VALUES (?, ?, ?)
             ON CONFLICT (email, fragment) DO UPDATE SET expires_at = excluded.expires_at",
            vec![i.email.as_str().into(), i.fragment.as_str().into(), SqlStorageValue::Integer(i.expires_at)],
        )?;
        let link = format!("{}/", self.cfg.outside_origin(&i.fragment));
        let days = limits::INVITE_TTL_S / (24 * 3600);
        let invite = mail::invite(&i.email, &from, &i.title, i.role == Role::Editor, &link, days);
        let sent = crate::mail::send(&self.env, self.cfg, &invite).await;
        if let Err(e) = sent {
            self.exec(
                "DELETE FROM invites_waiting WHERE email = ? AND fragment = ? AND expires_at = ?",
                vec![i.email.as_str().into(), i.fragment.as_str().into(), SqlStorageValue::Integer(i.expires_at)],
            )?;
            return Err(e);
        }
        Ok(Invited::Mailed)
    }

    /// Counts a mail in `sharer`'s day, or refuses it (429), counting
    /// nothing. Days before yesterday are forgotten.
    fn count_mail(&self, sharer: &str, now: i64) -> CellResult<()> {
        let day = now / DAY_MS;
        let mailed = self.count(
            "SELECT COALESCE(SUM(mailed), 0) AS n FROM invite_mails WHERE sharer = ? AND day = ?",
            vec![sharer.into(), SqlStorageValue::Integer(day)],
        )?;
        if mailed >= u64::from(limits::INVITE_MAILS_DAILY_MAX) {
            return Err(CellError::new(
                ErrorCode::RateLimited,
                format!("you have had {} invites mailed today, the most a day: invite more tomorrow", limits::INVITE_MAILS_DAILY_MAX),
            ));
        }
        self.exec(
            "INSERT INTO invite_mails (sharer, day, mailed) VALUES (?, ?, 1) ON CONFLICT (sharer, day) DO UPDATE SET mailed = mailed + 1",
            vec![sharer.into(), SqlStorageValue::Integer(day)],
        )?;
        self.exec("DELETE FROM invite_mails WHERE day < ?", vec![SqlStorageValue::Integer(day - 1)])
    }

    /// A sign-in that verified `email` for the person `identity` meets the
    /// invites waiting on it: each fragment makes them a member. A fragment
    /// that has none waiting (revoked, met, deleted) is forgotten; one that
    /// fails keeps its row for their next sign-in, and the sign-in goes on.
    pub(super) async fn meet_invites(&self, email: &str, identity: &str) -> CellResult<()> {
        let now = js::now_ms();
        self.exec("DELETE FROM invites_waiting WHERE expires_at <= ?", vec![SqlStorageValue::Integer(now)])?;
        let waiting = self.rows::<FragmentRow>(
            "SELECT fragment FROM invites_waiting WHERE email = ? ORDER BY expires_at LIMIT ?",
            vec![email.into(), SqlStorageValue::Integer(limits::INVITES_PER_EMAIL_MAX as i64)],
        )?;
        let claim = serde_json::to_value(ClaimInvite { email: email.to_string(), identity: identity.to_string() }).expect("a claim serializes");
        for batch in waiting.chunks(CLAIMS_AT_ONCE) {
            let answers = join_all(batch.iter().map(|w| crate::fragment::ask(&self.env, &w.fragment, "invites/claim", &claim))).await;
            for (w, answer) in batch.iter().zip(answers) {
                match answer {
                    Ok(_) => {}
                    Err(e) if e.code == ErrorCode::NotFound => {}
                    Err(e) => {
                        console_error!("an invite waiting on a sign-in's email at {} was not met ({:?}): {}", w.fragment, e.code, e.message);
                        continue;
                    }
                }
                self.exec("DELETE FROM invites_waiting WHERE email = ? AND fragment = ?", vec![email.into(), w.fragment.as_str().into()])?;
            }
        }
        Ok(())
    }
}
