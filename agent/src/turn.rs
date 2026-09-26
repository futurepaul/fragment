//! A turn is goose's state machine: load the conversation, run one step (a
//! steer, a batch of tool calls, or one model call), apply its effects in
//! SQL, repeat. A watchdog alarm stays armed while a turn runs, so a driver
//! that dies with its node is replaced and resumes from the last applied
//! step (ported from spikes/goose-agent, branch spike/goose-agent). The
//! model is called through model.rs, which bounds, retries, and names a
//! failed call; a turn that ends on one ends in an error (`ended`).
//!
//! wasm32 is single-threaded, so `SendFuture` (and model.rs `AssertSend`)
//! only satisfy goose's `Send` bounds; nothing crosses a thread.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use fragment_proto::TurnOutcome;
use async_trait::async_trait;
use goose_agent::inference::InferenceRunner;
use goose_agent::machine::{SessionLoader, StateMachine, Step};
use goose_agent::operation::{applied, ends_turn, last_effective_role, messages_since_kickoff, not_applicable, Emitter, Operation, OperationResult};
use goose_agent::tool::ToolOperation;
use goose_provider_types::base::Provider;
use goose_provider_types::conversation::message::{Message, MessageContent};
use goose_provider_types::conversation::{Conversation, EffectiveRole};
use rmcp::model::{CallToolResult, ContentBlock};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use worker::send::SendFuture;
use worker::{Delay, SqlStorage, Storage};

use crate::computer::{self, Computer, ComputerTools};
use crate::fleet::Fleet;
use crate::js;
use crate::model::{self, OpenRouter, Spend};
use crate::progress::Progress;
use crate::store::{self, kv_get, kv_set, kv_u64, Effect, Session, Store};
use crate::tools::{FragmentTools, Scope};

/// Every loop and input is bounded.
pub const STEPS_PER_TURN_MAX: u32 = 64;
pub const WATCHDOG_MS_DEFAULT: u64 = 30_000;
pub const WATCHDOG_MS_MIN: u64 = 2_000;
const EVENTS_BUFFER: usize = 1024;

pub struct Model {
    /// The OpenRouter base (`OPENROUTER_API_URL`, default https://openrouter.ai).
    pub base: String,
    pub name: String,
    /// One model call's deadline (model.rs `DEADLINE_MS`, or a test control's).
    pub deadline_ms: u64,
    /// Where its calls are paid (the owner's month).
    pub spend: Spend,
}

/// A computer attached to the agent, and the project directory its tools work in.
pub struct Attached {
    pub computer: Computer,
    pub cwd: String,
}

/// One turn's driver: its conversation, and whom it acts for.
pub struct Driver {
    /// The agent cell's, shared by the turns one driver runs.
    pub storage: Rc<Storage>,
    pub model: Model,
    /// The agent's own; the turn's tools act for `asker` through it.
    pub fleet: Fleet,
    pub instructions: String,
    pub computer: Option<Attached>,
    pub cancel: CancellationToken,
    pub id: String,
    /// The turn's conversation (store.rs `DIRECT`, or a chat's).
    pub conv: String,
    /// Who started the turn (an identity): every call acts for them.
    pub asker: String,
    /// Whether that is the agent's owner.
    pub owner_turn: bool,
    /// A chat turn's progress records (progress.rs), when its chat takes them.
    pub progress: Option<Rc<Progress>>,
    /// A fragment's own agent's: its tools are that fragment's (tools.rs).
    pub scope: Option<Scope>,
}

// ---------------------------------------------------------------- operations

struct Instructions(String);

#[async_trait]
impl Operation<Session, Effect> for Instructions {
    fn name(&self) -> &'static str {
        "instructions"
    }

    async fn prompt_parts(&self, _: &Session, _: &Conversation) -> anyhow::Result<Vec<(String, String)>> {
        Ok(vec![("instructions".into(), self.0.clone())])
    }
}

/// A call to a tool the turn does not offer (one the model recalls from an
/// earlier turn, such as a chat's reply operation, or makes up) is
/// answered with an error. goose's tool step leaves such a call for a
/// client to run, and nothing here would: the turn would end on it, its
/// answer never said. It runs after the tool step, so a call it finds
/// unanswered is one no tool took.
struct Unoffered;

#[async_trait]
impl Operation<Session, Effect> for Unoffered {
    fn name(&self) -> &'static str {
        "unoffered"
    }

    async fn run(&self, _: &Session, conversation: &Conversation, emit: &Emitter) -> anyhow::Result<OperationResult<Effect>> {
        let turn = messages_since_kickoff(conversation)?;
        let answered: HashSet<&str> = turn.iter().flat_map(Message::get_tool_response_ids).collect();
        let mut message = Message::user();
        for request in turn.iter().flat_map(|m| m.content.iter()).filter_map(MessageContent::as_tool_request) {
            if request.was_executed_externally() || answered.contains(request.id.as_str()) {
                continue;
            }
            let name = request.tool_call.as_ref().map(|c| c.name.to_string()).unwrap_or_default();
            let error = CallToolResult::error(vec![ContentBlock::text(format!("no tool named {name}"))]);
            message.add_tool_response_with_metadata(request.id.clone(), Ok(error), request.metadata.as_ref());
        }
        if message.get_tool_response_ids().is_empty() {
            return not_applicable();
        }
        let message = emit.message(message).await;
        applied([Effect::Message(message)])
    }
}

/// goose's steer operation (crates/goose/src/agents/state_machine/ops_steer.rs),
/// reading a durable queue instead of an in-memory one.
struct Steer {
    sql: SqlStorage,
}

#[async_trait]
impl Operation<Session, Effect> for Steer {
    fn name(&self) -> &'static str {
        "steer"
    }

    async fn run(&self, _: &Session, conversation: &Conversation, emit: &Emitter) -> anyhow::Result<OperationResult<Effect>> {
        let messages = messages_since_kickoff(conversation)?;
        let between_turns = ends_turn(messages) || last_effective_role(messages)? == EffectiveRole::Tool;
        if !between_turns {
            return not_applicable();
        }
        #[derive(Deserialize)]
        struct Row {
            seq: i64,
            text: String,
        }
        let pending: Vec<Row> = self
            .sql
            .exec("SELECT seq, text FROM steer WHERE consumed = 0 ORDER BY seq", None)
            .and_then(|c| c.to_array())
            .map_err(|e| anyhow!("{e}"))?;
        if pending.is_empty() {
            return not_applicable();
        }
        let mut effects = Vec::with_capacity(pending.len() + 1);
        let mut seqs = Vec::with_capacity(pending.len());
        for row in pending {
            let message = emit.message(Message::user().with_text(row.text).with_steer()).await;
            effects.push(Effect::Message(message));
            seqs.push(row.seq);
        }
        effects.push(Effect::ConsumeSteer(seqs));
        applied(effects)
    }
}

// ---------------------------------------------------------------- the driver

pub async fn arm_watchdog(storage: &Storage, sql: &SqlStorage) -> anyhow::Result<()> {
    let watchdog = match kv_u64(sql, "watchdog_ms")? {
        0 => WATCHDOG_MS_DEFAULT,
        ms => ms,
    };
    storage.set_alarm(Duration::from_millis(watchdog)).await.map_err(|e| anyhow!("{e}"))
}

async fn run_steps(driver: &Driver, machine: &StateMachine<'_, Session, Effect>, store: &Store, emit: Emitter) -> anyhow::Result<TurnOutcome> {
    let sql = &store.sql;
    // Arm the watchdog before any work, so a node that dies during the
    // first step still leaves a wake behind.
    arm_watchdog(&driver.storage, sql).await?;
    for _ in 0..STEPS_PER_TURN_MAX {
        if kv_u64(sql, "cancel")? == 1 {
            driver.cancel.cancel();
        }
        let t0 = js::now_ms();
        let session = store.load(&driver.conv).await?;
        let Some(mut result) = machine.step(&session, &emit).await? else {
            // a stopped turn is stopped (`drive`), whatever it last held
            if driver.cancel.is_cancelled() {
                return Ok(TurnOutcome::Idle);
            }
            return ended(&session.conversation);
        };
        machine.apply(store, &session, &mut result, &emit).await?;
        let name = result.applied_step.unwrap_or("?");
        let t1 = js::now_ms();
        store::record_step(sql, name, result.effects.len(), t1 - t0, t1, &driver.id)?;
        arm_watchdog(&driver.storage, sql).await?;
        // the steps that store tool results: their calls go to the chat's
        // `work` channel (best-effort), before any test hold below
        if let (Some(progress), "tools" | "unoffered") = (&driver.progress, name) {
            progress.steps().await;
        }
        if name == "tools" {
            // Test hook: hold after a tool result is persisted, so a kill
            // lands between steps.
            let hold = kv_u64(sql, "test_hold_after_tool_ms")?;
            if hold > 0 && !driver.cancel.is_cancelled() {
                futures::future::select(Box::pin(Delay::from(Duration::from_millis(hold))), Box::pin(driver.cancel.cancelled())).await;
            }
        }
        if result.yield_to_client {
            return Ok(TurnOutcome::Yielded);
        }
    }
    Err(anyhow!("the turn exceeded {STEPS_PER_TURN_MAX} steps"))
}

/// How a turn that has nothing left to do ended: answered (idle), or,
/// when its last message is goose's record of a failed model call (after
/// model.rs tried twice) or an answer with nothing in it, an error that
/// says why. Never silent: the caller says it where the person reads it.
fn ended(conversation: &Conversation) -> anyhow::Result<TurnOutcome> {
    let Some(last) = conversation.last() else { return Ok(TurnOutcome::Idle) };
    if let Some(error) = last.content.iter().find_map(MessageContent::as_error) {
        return Err(anyhow!("{}", model::reason(&error.message)));
    }
    let said = !last.as_concat_text().trim().is_empty() || last.is_tool_call();
    if last.role == rmcp::model::Role::Assistant && !said {
        return Err(anyhow!("the model's last answer was empty"));
    }
    Ok(TurnOutcome::Idle)
}

/// Runs the turn to its end (or its cancellation): the outcome is idle
/// (the model answered), yielded, stopped, or an error.
pub async fn drive(driver: Driver) -> anyhow::Result<TurnOutcome> {
    let sql = driver.storage.sql();
    // armed before reaching a computer, which can take its retries
    arm_watchdog(&driver.storage, &sql).await?;
    let store = Store { sql: driver.storage.sql() };
    let provider: Arc<dyn Provider> =
        Arc::new(OpenRouter { base: driver.model.base.clone(), deadline_ms: driver.model.deadline_ms, spend: driver.model.spend.clone() });
    let fleet = driver.fleet.acting_for(&driver.asker);
    let tools = FragmentTools::new(fleet, driver.storage.sql(), driver.id.clone(), driver.conv.clone(), driver.owner_turn, driver.scope.clone());
    let mut operation = ToolOperation::new().with_provider(Arc::new(tools));
    let mut instructions = driver.instructions.clone();
    let in_flight = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    // the computer is its owner's: someone else's turn never reaches it
    assert!(driver.computer.is_none() || driver.owner_turn, "a computer joins its owner's turns only");
    if let Some(attached) = &driver.computer {
        // the computer's tools join the fragments' for this turn
        let c = attached.computer.clone();
        let manifest = SendFuture::new(async move { c.get("/tools").await }).await?;
        instructions.push_str(&format!(
            "\n\nYou also have a computer. Its tools (shell, write, edit, tree) start in your project directory there \
             (`{}`): give paths relative to it (`index.html`, not `{}/index.html`).\n\n{}",
            attached.cwd,
            attached.cwd,
            manifest["instructions"].as_str().unwrap_or("")
        ));
        operation = operation.with_provider(Arc::new(ComputerTools {
            computer: attached.computer.clone(),
            cwd: attached.cwd.clone(),
            tools: computer::tools_of(&manifest)?,
            sql: driver.storage.sql(),
            driver: driver.id.clone(),
            in_flight: in_flight.clone(),
        }));
    }
    let machine: StateMachine<Session, Effect> = StateMachine::new(
        vec![
            Step::Operation(Arc::new(operation)),
            Step::Operation(Arc::new(Unoffered)),
            Step::Operation(Arc::new(Steer { sql: driver.storage.sql() })),
            Step::Operation(Arc::new(Instructions(instructions))),
            Step::Inference(Arc::new(InferenceRunner::new(provider, model::config(&driver.model.name)))),
        ],
        driver.cancel.clone(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(EVENTS_BUFFER);
    let emit = Emitter::new(tx, driver.cancel.clone());
    let drain = async move {
        let mut events = 0u64;
        while rx.recv().await.is_some() {
            events += 1;
        }
        events
    };
    let (outcome, events) = futures::join!(run_steps(&driver, &machine, &store, emit), drain);
    kv_set(&sql, "events", kv_u64(&sql, "events")? + events)?;
    let left: Vec<String> = std::mem::take(&mut *in_flight.lock().expect("in-flight lock")).into_iter().collect();
    if let (Some(attached), false) = (&driver.computer, left.is_empty()) {
        let c = attached.computer.clone();
        SendFuture::new(async move { c.cancel_all(left).await }).await;
    }
    if kv_get(&sql, "cancel")?.as_deref() == Some("1") && outcome.is_ok() {
        return Ok(TurnOutcome::Stopped);
    }
    outcome
}
