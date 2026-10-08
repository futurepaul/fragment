//! The note a turn carries after a cut one (docs/durable-computers.md, P5):
//! the first turn an agent runs in a chat after one of its turns there was
//! ended as lost (a restart cut it; P1, one life per turn, never runs it
//! again) is told so, with what that turn was asked, the steps and replies
//! the journal recorded of it, and to check what was done before doing any
//! of it again.
//!
//! Built from the chat's journal alone: `work` (the turns, their steps and
//! cards) and `chat` (what was asked, what was replied), which never go
//! back in time, so a note is the same in every life and after any rollback
//! or loss of `/data`. Pure: records in, a note out; the driver reads the
//! records (driver.rs, `note_for`), and runtimes are handed the text
//! (`TurnStart::note`).
//!
//! The rule: the agent's turn before this one in this chat ended as lost.
//! "Before" is the journal's order: the latest of the agent's `turn.start`
//! records before this turn's own (its claim), passing over a refusal's,
//! which never ran. At the turn after, the turn before is this one, so the
//! note is said once. A turn before it with no end in the journal (its end
//! never landed) is told nothing: only a recorded loss is a cut.

use serde_json::Value;

use crate::engine::{LOST, REFUSED_BUSY, REFUSED_QUEUED};
use crate::limits;
use crate::records::{self, Cause, Record, Said, Task};

/// One step a cut turn had recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutStep {
    pub tool: String,
    pub args: String,
    pub ok: bool,
}

/// A card a cut turn had shown, and how it closed (`None`: never closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutCard {
    pub text: String,
    pub closed: Option<String>,
}

/// A turn of the agent's that a restart cut, as its chat's `work` recorded
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub turn: String,
    /// The record that started it: what it was asked.
    pub cause: Cause,
    /// When its claim was recorded (the platform's `at`, ms): its replies
    /// on `chat` come after.
    pub started_at: i64,
    /// Its steps in order, at most `NOTE_STEPS_MAX`, and how many it had.
    pub steps: Vec<CutStep>,
    pub steps_total: usize,
    /// Its cards in order, at most `NOTE_CARDS_MAX`.
    pub cards: Vec<CutCard>,
}

/// What the journal before a turn's claim says of the agent's turn before
/// it in the chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Before {
    /// No turn of the agent's in the records read: read further back.
    Unread,
    /// Its turn before this one did not end as lost (or there was none
    /// before the first record: `previous` is told the records start there).
    Kept,
    /// Its turn before this one was cut by a restart.
    Cut(Cut),
}

fn field<'a>(b: &'a Value, k: &str) -> &'a str {
    b[k].as_str().unwrap_or("")
}

/// The agent's own record of `kind` (posted as it, naming it where the
/// record names an agent).
fn own<'a>(r: &'a Record, agent: &str, kind: &str) -> Option<&'a Value> {
    (r.principal == agent && r.body["kind"] == kind).then_some(&r.body)
}

/// Whether a `turn.end` is a refusal: the turn never ran.
fn refused(end: &Value) -> bool {
    end["outcome"] == "error" && matches!(field(end, "error"), REFUSED_QUEUED | REFUSED_BUSY)
}

/// The agent's turn before a claim, from `work`: a run of the chat's `work`
/// records ending just before the claim, oldest first (the claim itself and
/// anything after it left out). `from_start`: the run begins at the
/// channel's first record, so a turn found nowhere in it means none.
/// `agent` is the agent's identity.
pub fn previous(work: &[Record], agent: &str, from_start: bool) -> Before {
    // newest first: a turn's end comes after its start, so it is met first
    let mut ends: Vec<(&str, &Value)> = Vec::new();
    for (i, r) in work.iter().enumerate().rev() {
        if let Some(end) = own(r, agent, "turn.end") {
            ends.push((field(end, "turn"), end));
            continue;
        }
        let Some(start) = own(r, agent, "turn.start") else { continue };
        if start["agent"] != agent {
            continue;
        }
        let turn = field(start, "turn");
        let end = ends.iter().find(|(t, _)| *t == turn).map(|(_, e)| *e);
        match end {
            Some(e) if refused(e) => continue,
            Some(e) if e["outcome"] == "error" && field(e, "error") == LOST => {
                return cut(&work[i..], agent, turn, start, r.at).map_or(Before::Kept, Before::Cut);
            }
            _ => return Before::Kept,
        }
    }
    if from_start {
        Before::Kept
    } else {
        Before::Unread
    }
}

/// The cut turn `turn` as the records from its start on hold it.
fn cut(from: &[Record], agent: &str, turn: &str, start: &Value, started_at: i64) -> Option<Cut> {
    let cause: Cause = serde_json::from_value(start["cause"].clone()).ok()?;
    let mine = |r: &&Record| r.principal == agent && r.body["turn"] == turn;
    let steps: Vec<CutStep> = from
        .iter()
        .filter(mine)
        .filter(|r| r.body["kind"] == "turn.step")
        .map(|r| CutStep { tool: field(&r.body, "tool").to_string(), args: field(&r.body, "args").to_string(), ok: r.body["ok"] != false })
        .collect();
    let steps_total = steps.len();
    let closed = |prompt: &str| -> Option<String> {
        let c = from.iter().filter(mine).map(|r| &r.body).find(|b| b["kind"] == "turn.prompt.closed" && b["prompt"] == prompt)?;
        Some(match field(c, "outcome") {
            "answered" => format!("answered: {}", field(c, "option")),
            other => other.to_string(),
        })
    };
    let cards: Vec<CutCard> = from
        .iter()
        .filter(mine)
        .filter(|r| r.body["kind"] == "turn.prompt")
        .take(limits::NOTE_CARDS_MAX)
        .map(|r| CutCard { text: field(&r.body, "text").to_string(), closed: closed(field(&r.body, "prompt")) })
        .collect();
    Some(Cut { turn: turn.to_string(), cause, started_at, steps: steps.into_iter().take(limits::NOTE_STEPS_MAX).collect(), steps_total, cards })
}

/// What a cause record asked: a message's text (or what it carried), or a
/// routine's.
pub fn asked(cause: &Record) -> Option<String> {
    let text = match cause.channel.as_str() {
        records::CHAT => match records::said(&cause.body) {
            Said::Message(m) if !m.text.trim().is_empty() => m.text,
            Said::Message(m) if !m.attachments.is_empty() => format!("({} attached files)", m.attachments.len()),
            _ => return None,
        },
        records::TASKS => match records::task(&cause.body) {
            Task::Routine { text, .. } => format!("your routine: {text}"),
            _ => return None,
        },
        _ => return None,
    };
    Some(text)
}

/// What the agent had replied in `turn`, from `chat` records, in order.
pub fn replies(chat: &[Record], agent: &str, turn: &str) -> Vec<String> {
    chat.iter()
        .filter(|r| r.principal == agent && r.body["turn"] == turn)
        .filter_map(|r| match records::said(&r.body) {
            Said::Message(m) if !m.text.trim().is_empty() => Some(m.text),
            Said::Message(m) if !m.attachments.is_empty() => Some(format!("({} files)", m.attachments.len())),
            _ => None,
        })
        .collect()
}

/// One line of the note, quoted and on one line.
fn quoted(text: &str, max: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("“{}”", records::cut(&one_line, max))
}

/// The note, bounded (`NOTE_MAX_BYTES`): what it is and what to do first,
/// then what the cut turn was asked, its steps and cards, and what it had
/// replied.
pub fn text(cut: &Cut, asked: Option<&str>, replies: &[String]) -> String {
    let mut lines = vec![
        "Your previous turn in this chat was cut short: your computer restarted before it finished. Check what it already did before you do any of it again; the message below is new.".to_string(),
    ];
    if let Some(a) = asked.filter(|a| !a.trim().is_empty()) {
        lines.push(format!("It was answering: {}", quoted(a, limits::NOTE_ASKED_MAX_CHARS)));
    }
    if !cut.steps.is_empty() {
        let mut steps: Vec<String> = cut
            .steps
            .iter()
            .map(|s| {
                let args = s.args.split_whitespace().collect::<Vec<_>>().join(" ");
                let args = if args.is_empty() { String::new() } else { format!(" {}", records::cut(&args, limits::NOTE_ARGS_MAX_CHARS)) };
                let tool = if s.tool.trim().is_empty() { "a step".to_string() } else { records::cut(&s.tool, limits::NOTE_TOOL_MAX_CHARS) };
                format!("{tool}{args} ({})", if s.ok { "ok" } else { "failed" })
            })
            .collect();
        if cut.steps_total > cut.steps.len() {
            steps.push(format!("and {} more", cut.steps_total - cut.steps.len()));
        }
        lines.push(format!("Its steps, as recorded: {}", steps.join("; ")));
    }
    for c in &cut.cards {
        let closed = c.closed.as_deref().unwrap_or("unanswered");
        lines.push(format!("It asked: {} ({closed})", quoted(&c.text, limits::NOTE_CARD_MAX_CHARS)));
    }
    let shown = replies.len().min(limits::NOTE_REPLIES_MAX);
    for r in &replies[replies.len() - shown..] {
        lines.push(format!("It had replied: {}", quoted(r, limits::NOTE_REPLY_MAX_CHARS)));
    }
    let note = records::cut_bytes(&lines.join("\n"), limits::NOTE_MAX_BYTES);
    assert!(note.len() <= limits::NOTE_MAX_BYTES, "a note is bounded");
    note
}

/// One of the agent's turns in a chat that another life ran since the
/// `/data` this life restored ("forgotten", below): what it was asked, and
/// what it replied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forgot {
    pub turn: String,
    pub asked: Option<String>,
    pub replies: Vec<String>,
}

/// The agent's turns `turns` (oldest first) as the chat's journal holds
/// them: each one's start on `work` (its cause, which `chat` holds when it
/// is this chat's message) and its replies on `chat`. A turn whose start
/// is not among the records read is left out (nothing of it can be said),
/// and so is one a restart cut: it is the agent's turn before this one
/// (one turn of an agent runs in a chat at a time, and the cut one was its
/// life's last), whose own note says what was cut (`previous`, `text`).
/// Pure, as `previous` is.
///
/// "Forgotten": a rollback of `/data` sends the bridge's cursors back, and
/// the turns it reads again were claimed by the life that ran them (their
/// claims answer 409), so its runtime, restored from the older save,
/// remembers none of them. The agent's next turn in the chat is told what
/// they were, from the journal, which never goes back in time.
pub fn forgotten(work: &[Record], chat: &[Record], agent: &str, turns: &[String]) -> Vec<Forgot> {
    let mut out = Vec::with_capacity(turns.len());
    for turn in turns {
        let start = work.iter().find(|r| own(r, agent, "turn.start").is_some_and(|b| b["turn"] == turn.as_str() && b["agent"] == agent));
        let Some(start) = start else { continue };
        let Ok(cause) = serde_json::from_value::<Cause>(start.body["cause"].clone()) else { continue };
        let cut = work.iter().filter_map(|r| own(r, agent, "turn.end")).any(|e| e["turn"] == turn.as_str() && e["outcome"] == "error" && field(e, "error") == LOST);
        if cut {
            continue;
        }
        let asked = chat.iter().find(|r| r.channel == records::CHAT && r.seq == cause.seq && cause.channel == records::CHAT).and_then(asked);
        out.push(Forgot { turn: turn.clone(), asked, replies: replies(chat, agent, turn) });
    }
    assert!(out.len() <= turns.len(), "each forgotten turn is told once at most");
    out
}

/// What the agent is told of its forgotten turns in the chat, bounded
/// (`NOTE_MAX_BYTES`): what happened and what to do first, then each
/// turn's request and its last replies, oldest first, and how many earlier
/// ones there were.
pub fn forgotten_text(items: &[Forgot], more: u32) -> String {
    let mut lines = vec![
        "Your memory of this chat is behind: your computer went back to an earlier save, so you do not remember these turns of yours here, which came after it (the chat keeps them). What they did may have had effects: check before you do any of it again.".to_string(),
    ];
    if more > 0 {
        lines.push(format!("(And {more} earlier.)"));
    }
    for f in items {
        let asked = f.asked.as_deref().filter(|a| !a.trim().is_empty()).map_or_else(|| "something".to_string(), |a| quoted(a, limits::NOTE_ASKED_MAX_CHARS));
        lines.push(format!("You were asked: {asked}"));
        let shown = f.replies.len().min(limits::NOTE_REPLIES_MAX);
        for r in &f.replies[f.replies.len() - shown..] {
            lines.push(format!("You replied: {}", quoted(r, limits::NOTE_REPLY_MAX_CHARS)));
        }
    }
    let note = records::cut_bytes(&lines.join("\n"), limits::NOTE_MAX_BYTES);
    assert!(note.len() <= limits::NOTE_MAX_BYTES, "a note is bounded");
    note
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::records::{turn_end, turn_prompt_closed, turn_start, turn_step, Closed, Outcome, Step};

    const ME: &str = "npub1juniper";
    const LIFE: &str = "0123456789abcdef0123456789abcdef";

    /// A `work` channel, as the agent `ME` (and another agent) post it.
    #[derive(Default)]
    struct Work(Vec<Record>);

    impl Work {
        fn post(&mut self, by: &str, body: Value) -> u64 {
            let seq = self.0.len() as u64 + 1;
            self.0.push(Record { channel: "work".into(), seq, at: seq as i64 * 10, principal: by.into(), kind: "message".into(), body });
            seq
        }

        fn start(&mut self, by: &str, turn: &str, seq: u64) -> u64 {
            let cause = Cause { fragment: "talk.paul".into(), channel: "chat".into(), seq };
            self.post(by, turn_start(turn, "npub1paul", by, &cause, LIFE))
        }

        fn end(&mut self, by: &str, turn: &str, outcome: Outcome) -> u64 {
            self.post(by, turn_end(turn, &outcome))
        }

        /// The records before `seq` (a claim's), as the driver reads them.
        fn before(&self, seq: u64) -> &[Record] {
            &self.0[..usize::try_from(seq - 1).unwrap()]
        }
    }

    fn step(tool: &str, args: &str, ok: bool) -> Step {
        Step { tool: tool.into(), args: args.into(), ok, excerpt: String::new(), text: String::new() }
    }

    /// Goal (P5): the turn after a cut one is told of it, with its steps and
    /// its card as the journal recorded them, and the turn after that is told
    /// nothing: the note is said once.
    #[test]
    fn the_turn_after_a_cut_one_is_told_once() {
        let mut w = Work::default();
        w.start(ME, "t1", 1);
        w.end(ME, "t1", Outcome::Idle);
        w.start(ME, "cut", 3);
        w.post(ME, turn_step("cut", 1, &step("terminal", "`ls -la`", true)));
        w.post("npub1rowan", turn_step("other", 1, &step("search", "not mine", true)));
        w.post(ME, turn_step("cut", 2, &step("terminal", "rm -rf /tmp/x", false)));
        w.post(ME, json!({ "kind": "turn.prompt", "turn": "cut", "prompt": "p1", "text": "Run `rm -rf /tmp/x`?", "options": [], "asks": "npub1paul", "expiresAt": 1 }));
        w.post(ME, turn_prompt_closed("cut", "p1", Closed::Expired, None));
        w.end(ME, "cut", Outcome::Error(LOST.into()));
        let next = w.start(ME, "next", 9);

        let Before::Cut(c) = previous(w.before(next), ME, true) else { panic!("the turn after a cut one is told of it") };
        assert_eq!(c.turn, "cut");
        assert_eq!(c.cause, Cause { fragment: "talk.paul".into(), channel: "chat".into(), seq: 3 });
        assert_eq!(c.steps, vec![CutStep { tool: "terminal".into(), args: "`ls -la`".into(), ok: true }, CutStep { tool: "terminal".into(), args: "rm -rf /tmp/x".into(), ok: false }], "its own steps, in order, another agent's left out");
        assert_eq!(c.cards, vec![CutCard { text: "Run `rm -rf /tmp/x`?".into(), closed: Some("expired".into()) }]);
        let note = text(&c, Some("do the risky thing"), &["Starting.".into()]);
        assert_eq!(
            note,
            "Your previous turn in this chat was cut short: your computer restarted before it finished. Check what it already did before you do any of it again; the message below is new.\n\
             It was answering: “do the risky thing”\n\
             Its steps, as recorded: terminal `ls -la` (ok); terminal rm -rf /tmp/x (failed)\n\
             It asked: “Run `rm -rf /tmp/x`?” (expired)\n\
             It had replied: “Starting.”"
        );

        // the turn after: the turn before it is `next`, which was not cut
        w.end(ME, "next", Outcome::Idle);
        let after = w.start(ME, "after", 11);
        assert_eq!(previous(w.before(after), ME, true), Before::Kept, "said once");
        // and the cut turn's own claim is told of the turn before it, not of itself
        assert_eq!(previous(w.before(3), ME, true), Before::Kept);
    }

    /// Goal: only a recorded loss is a cut. A turn that ended any other way,
    /// one whose end never landed, or none at all, is told nothing; a
    /// refusal (it never ran) is passed over, so the turn after a cut one
    /// is told of it even with a refusal between; another agent's cut turn
    /// is not this agent's.
    #[test]
    fn only_a_recorded_loss_is_told() {
        for outcome in [Outcome::Idle, Outcome::Stopped, Outcome::Error("the agent stopped answering".into())] {
            let mut w = Work::default();
            w.start(ME, "t", 1);
            w.end(ME, "t", outcome.clone());
            let n = w.start(ME, "n", 2);
            assert_eq!(previous(w.before(n), ME, true), Before::Kept, "{outcome:?}");
        }
        let mut w = Work::default();
        w.start(ME, "t", 1);
        let n = w.start(ME, "n", 2);
        assert_eq!(previous(w.before(n), ME, true), Before::Kept, "no end landed: no recorded loss");
        assert_eq!(previous(&[], ME, true), Before::Kept, "the first turn");
        assert_eq!(previous(&[], ME, false), Before::Unread, "nothing read yet");

        let mut w = Work::default();
        w.start(ME, "cut", 1);
        w.end(ME, "cut", Outcome::Error(LOST.into()));
        for (i, why) in [REFUSED_QUEUED, REFUSED_BUSY].into_iter().enumerate() {
            let t = format!("refused{i}");
            w.start(ME, &t, 2 + i as u64);
            w.end(ME, &t, Outcome::Error(why.into()));
        }
        w.start("npub1rowan", "rowans", 5);
        w.end("npub1rowan", "rowans", Outcome::Error(LOST.into()));
        let n = w.start(ME, "n", 6);
        assert!(matches!(previous(w.before(n), ME, true), Before::Cut(c) if c.turn == "cut"), "refusals and another agent's turns passed over");
        assert!(matches!(previous(w.before(n), "npub1rowan", true), Before::Cut(c) if c.turn == "rowans"), "rowan's turn before is rowan's own");

        // a forged start (another principal naming this agent) is no turn of its
        let mut w = Work::default();
        let cause = Cause { fragment: "talk.paul".into(), channel: "chat".into(), seq: 1 };
        w.post("npub1mallory", turn_start("forged", "npub1paul", ME, &cause, LIFE));
        w.post("npub1mallory", turn_end("forged", &Outcome::Error(LOST.into())));
        let n = w.start(ME, "n", 2);
        assert_eq!(previous(w.before(n), ME, true), Before::Kept);
    }

    /// Goal: a run of records that does not reach back to the agent's turn
    /// before says so (`Unread`), so the driver reads further back; once it
    /// does, the answer is the whole channel's.
    #[test]
    fn a_short_read_reads_further_back() {
        let mut w = Work::default();
        w.start(ME, "cut", 1);
        w.end(ME, "cut", Outcome::Error(LOST.into()));
        for i in 0..5 {
            w.start("npub1rowan", &format!("r{i}"), 10 + i);
            w.end("npub1rowan", &format!("r{i}"), Outcome::Idle);
        }
        let n = w.start(ME, "n", 20);
        let before = w.before(n);
        assert_eq!(previous(&before[4..], ME, false), Before::Unread, "the tail holds only rowan's");
        // the end alone, its start not read yet, is no answer either
        assert_eq!(previous(&before[1..], ME, false), Before::Unread);
        assert!(matches!(previous(before, ME, false), Before::Cut(c) if c.turn == "cut"));
    }

    /// Goal: a note is bounded whatever the cut turn did (a long request,
    /// many steps, long replies), and its first line, what it is and what
    /// to do, is always whole.
    #[test]
    fn a_note_is_bounded() {
        let mut w = Work::default();
        w.start(ME, "cut", 1);
        for n in 1..=150 {
            w.post(ME, turn_step("cut", n, &step(&"t".repeat(140), &"a".repeat(140), true)));
        }
        for n in 0..20 {
            w.post(ME, json!({ "kind": "turn.prompt", "turn": "cut", "prompt": format!("p{n}"), "text": "x".repeat(2000), "options": [], "asks": "npub1paul", "expiresAt": 1 }));
        }
        w.end(ME, "cut", Outcome::Error(LOST.into()));
        let n = w.start(ME, "n", 2);
        let Before::Cut(c) = previous(w.before(n), ME, true) else { panic!("cut") };
        assert_eq!((c.steps.len(), c.steps_total, c.cards.len()), (limits::NOTE_STEPS_MAX, 150, limits::NOTE_CARDS_MAX));
        // as long as the records allow, ASCII: every part fits whole
        let long = "x".repeat(10_000);
        let note = text(&c, Some(&long), &vec![long.clone(); 10]);
        assert!(note.len() <= limits::NOTE_MAX_BYTES, "{}", note.len());
        assert!(note.contains("and 142 more") && note.lines().filter(|l| l.starts_with("It had replied: ")).count() == limits::NOTE_REPLIES_MAX && !note.ends_with('…'), "{note}");
        // far from ASCII: the tail is cut, marked, and the first line is whole
        let long = "é".repeat(10_000);
        let note = text(&c, Some(&long), &vec![long.clone(); 10]);
        assert!(note.len() <= limits::NOTE_MAX_BYTES, "{}", note.len());
        assert!(note.starts_with("Your previous turn in this chat was cut short") && note.lines().next().unwrap().ends_with("the message below is new."));
        // what was asked, read strictly from its record
        let msg = Record { channel: "chat".into(), seq: 1, at: 0, principal: "npub1paul".into(), kind: "message".into(), body: json!({ "text": "do\nthe risky thing" }) };
        assert_eq!(asked(&msg).as_deref(), Some("do\nthe risky thing"));
        let routine = Record { channel: "tasks".into(), body: json!({ "kind": "routine", "text": "water the plants", "chat": "talk.paul" }), ..msg.clone() };
        assert_eq!(asked(&routine).as_deref(), Some("your routine: water the plants"));
        assert_eq!(asked(&Record { body: json!({ "kind": "stop" }), ..msg.clone() }), None);
        let reply = |turn: &str, by: &str, text: &str| Record { principal: by.into(), body: json!({ "text": text, "turn": turn }), ..msg.clone() };
        assert_eq!(replies(&[reply("cut", ME, "one"), reply("other", ME, "x"), reply("cut", "npub1rowan", "y"), reply("cut", ME, "two")], ME, "cut"), vec!["one".to_string(), "two".to_string()]);
        // on one line, so the note's lines stay its own
        assert!(text(&c, Some("a\n\nb"), &[]).contains("It was answering: “a b”"));
    }

    /// Goal (a rollback): the turns another life ran that this life's
    /// runtime does not remember are told from the journal, oldest first:
    /// what each was asked and what it replied; one a restart cut is the
    /// cut note's, another agent's and a turn the records read do not hold
    /// are left out; the note is bounded, its first line whole. Valid,
    /// invalid.
    #[test]
    fn forgotten_turns_are_told_from_the_journal() {
        let msg = |seq: u64, by: &str, body: Value| Record { channel: "chat".into(), seq, at: seq as i64, principal: by.into(), kind: "message".into(), body };
        let chat = vec![
            msg(1, "id:paul", json!({ "text": "remember the zebra" })),
            msg(2, ME, json!({ "text": "noted: zebra", "turn": "t1" })),
            msg(3, "id:paul", json!({ "text": "and the\ngiraffe" })),
            msg(4, ME, json!({ "text": "noted: giraffe", "turn": "t2" })),
            msg(5, ME, json!({ "text": "both noted", "turn": "t2" })),
            msg(6, "id:paul", json!({ "text": "a slow one" })),
            msg(7, "id:paul", json!({ "text": "rowan, hi" })),
        ];
        let mut w = Work::default();
        w.start(ME, "t1", 1);
        w.end(ME, "t1", Outcome::Idle);
        w.start(ME, "t2", 3);
        w.end(ME, "t2", Outcome::Idle);
        w.start(ME, "cut", 6);
        w.end(ME, "cut", Outcome::Error(LOST.into()));
        w.start("id:rowan", "r1", 7);
        let turns: Vec<String> = ["t1", "t2", "cut", "r1", "nowhere"].iter().map(|t| t.to_string()).collect();
        let got = forgotten(&w.0, &chat, ME, &turns);
        assert_eq!(
            got,
            vec![
                Forgot { turn: "t1".into(), asked: Some("remember the zebra".into()), replies: vec!["noted: zebra".into()] },
                Forgot { turn: "t2".into(), asked: Some("and the\ngiraffe".into()), replies: vec!["noted: giraffe".into(), "both noted".into()] },
            ],
            "its own, in order; the cut one (its own note's), another agent's and one not read left out"
        );
        let note = forgotten_text(&got, 3);
        assert_eq!(
            note,
            "Your memory of this chat is behind: your computer went back to an earlier save, so you do not remember these turns of yours here, which came after it (the chat keeps them). What they did may have had effects: check before you do any of it again.\n\
             (And 3 earlier.)\n\
             You were asked: “remember the zebra”\n\
             You replied: “noted: zebra”\n\
             You were asked: “and the giraffe”\n\
             You replied: “noted: giraffe”\n\
             You replied: “both noted”"
        );
        assert!(forgotten(&w.0, &chat, "id:nobody", &turns).is_empty(), "none of another's");
        // bounded, its first line whole
        let long = Forgot { turn: "t".into(), asked: Some("é".repeat(10_000)), replies: vec!["é".repeat(10_000); 5] };
        let note = forgotten_text(&vec![long; limits::NOTE_FORGOTTEN_MAX], u32::MAX);
        assert!(note.len() <= limits::NOTE_MAX_BYTES && note.lines().next().unwrap().ends_with("before you do any of it again."), "{}", note.len());
        assert!(forgotten_text(&[Forgot { turn: "t".into(), asked: None, replies: vec![] }], 0).ends_with("You were asked: something"));
    }
}
