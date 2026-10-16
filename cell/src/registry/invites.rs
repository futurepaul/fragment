//! Invites by email (docs/cloudflare-v1.md, decision 48). A fragment shares
//! with an email: the person who holds it is a member at once. When no one
//! does yet, the invite waits on the email in the fragment (members.rs),
//! and here, kept against the email, which the platform mails a link to
//! the fragment. A sign-in that verifies the email meets every invite
//! waiting on it: each fragment makes its person a member
//! (`invites/claim`). Signing in never makes a share; it meets one
//! addressed to its email. The mails a person has sent are capped by the
//! day.
//!
//! A claim that fails (the fragment unreachable, or full) keeps its
//! invite's row, and its try is kept (`invite_claims`): the registry's
//! alarm tries it again for whoever holds the email then, with the
//! outboxes' backoff, at most `CLAIM_TRIES_MAX` tries in all
//! (`fragment_core::backoff::claim_retry_ms`). Past them it waits on its
//! person's next sign-in, which tries it afresh.

use fragment_core::{backoff, mail};
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
CREATE TABLE IF NOT EXISTS invite_claims (
  email TEXT NOT NULL, fragment TEXT NOT NULL, due INTEGER NOT NULL, failed INTEGER NOT NULL, PRIMARY KEY (email, fragment));
CREATE INDEX IF NOT EXISTS invite_claims_due ON invite_claims (due);
";

const DAY_MS: i64 = 24 * 3600 * 1000;

/// Fragments a sign-in, or a run of the alarm, tells at once that it met
/// their invites.
const CLAIMS_AT_ONCE: usize = 32;

/// How soon the alarm's kept claims run again after a run that failed.
const CLAIMS_LATER_MS: i64 = 60_000;

#[derive(Deserialize)]
struct FragmentRow {
    fragment: String,
}

/// A kept claim that is due, as the alarm reads it: whether its invite
/// still waits, and who holds its email now.
#[derive(Deserialize)]
struct KeptRow {
    email: String,
    fragment: String,
    failed: i64,
    waits: i64,
    identity: Option<String>,
}

#[derive(Deserialize)]
struct DueRow {
    due: Option<i64>,
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
    /// fails keeps its row, and its claim is kept for the alarm (this try
    /// its first), and the sign-in goes on.
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
                self.claimed(email, &w.fragment, 0, answer)?;
            }
        }
        self.arm_claims().await
    }

    /// What a claim's answer leaves, `failed` tries having failed before it:
    /// met, or none waiting there, its invite is forgotten here, and its
    /// kept claim with it; failed, its claim is kept, due after its
    /// backoff, or, past its tries, left to its person's next sign-in.
    fn claimed(&self, email: &str, fragment: &str, failed: i64, answer: CellResult<serde_json::Value>) -> CellResult<()> {
        let key = || vec![SqlStorageValue::from(email), SqlStorageValue::from(fragment)];
        let failure = match answer {
            Ok(_) => None,
            Err(e) if e.code == ErrorCode::NotFound => None,
            Err(e) => Some(e),
        };
        if let Some(e) = failure {
            let failed = failed.saturating_add(1);
            console_error!("{}", serde_json::json!({ "invite-claim": fragment, "tries": failed, "failed": e.message, "code": e.code }));
            return match backoff::claim_retry_ms(failed) {
                Some(wait) => self.exec(
                    "INSERT INTO invite_claims (email, fragment, due, failed) VALUES (?, ?, ?, ?)
                     ON CONFLICT (email, fragment) DO UPDATE SET due = excluded.due, failed = excluded.failed",
                    vec![email.into(), fragment.into(), SqlStorageValue::Integer(js::now_ms() + wait), SqlStorageValue::Integer(failed)],
                ),
                None => self.exec("DELETE FROM invite_claims WHERE email = ? AND fragment = ?", key()),
            };
        }
        self.exec("DELETE FROM invites_waiting WHERE email = ? AND fragment = ?", key())?;
        self.exec("DELETE FROM invite_claims WHERE email = ? AND fragment = ?", key())
    }

    /// The alarm's claims: a batch of the kept ones that are due, each for
    /// whoever holds its email now, all at once. One whose invite waits no
    /// more (met or expired), or whose email no one holds now (their wipe
    /// ended them), or whose person a wipe of whom runs, is forgotten: its
    /// invite, if it waits, waits on a sign-in again. Then the alarm is
    /// armed for the next.
    pub(super) async fn claims_alarm(&self) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        let due = self.rows::<KeptRow>(
            "SELECT c.email, c.fragment, c.failed,
                    EXISTS (SELECT 1 FROM invites_waiting w WHERE w.email = c.email AND w.fragment = c.fragment AND w.expires_at > ?) AS waits,
                    (SELECT identity FROM subjects WHERE email = c.email LIMIT 1) AS identity
             FROM invite_claims c WHERE c.due <= ? ORDER BY c.due LIMIT ?",
            vec![now.clone(), now, SqlStorageValue::Integer(CLAIMS_AT_ONCE as i64)],
        )?;
        let mut tried = Vec::with_capacity(due.len());
        for kept in due {
            match kept.identity {
                Some(person) if kept.waits != 0 && !self.wiping(&person)? => tried.push((kept.email, kept.fragment, kept.failed, person)),
                _ => self.exec("DELETE FROM invite_claims WHERE email = ? AND fragment = ?", vec![kept.email.as_str().into(), kept.fragment.as_str().into()])?,
            }
        }
        // bounded: at most CLAIMS_AT_ONCE claims a run
        let answers = join_all(tried.iter().map(|(email, fragment, _, person)| {
            let claim = serde_json::to_value(ClaimInvite { email: email.clone(), identity: person.clone() }).expect("a claim serializes");
            async move { crate::fragment::ask(&self.env, fragment, "invites/claim", &claim).await }
        }))
        .await;
        for ((email, fragment, failed, _), answer) in tried.iter().zip(answers) {
            self.claimed(email, fragment, *failed, answer)?;
        }
        self.arm_claims().await
    }

    /// Arms the alarm for the earliest kept claim, unless it is armed sooner.
    pub(super) async fn arm_claims(&self) -> CellResult<()> {
        match self.row::<DueRow>("SELECT MIN(due) AS due FROM invite_claims", vec![])?.and_then(|r| r.due) {
            Some(due) => self.arm_at(due).await,
            None => Ok(()),
        }
    }

    /// After a failed run: the next one soon, unless one is armed sooner.
    pub(super) async fn claims_later(&self) -> CellResult<()> {
        self.arm_at(js::now_ms() + CLAIMS_LATER_MS).await
    }
}
