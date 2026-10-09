//! Goose's WASM GDK state machine inside an app facet. The host supplies
//! checkpointed model, tool and log effects; it does not drive the loop.
//! The wire messages stay intact beside Goose's typed conversation so
//! cache hints and opaque provider thinking blocks survive unchanged.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use goose_agent::inference::{InferenceEffect, InferenceRunner};
use goose_agent::machine::{EffectHandler, MachineSession, SessionLoader, StateMachine, Step};
use goose_agent::operation::{Emitter, MachineEffect};
use goose_agent::tool::{ToolOperation, ToolProvider};
use goose_provider_types::base::{MessageStream, Provider};
use goose_provider_types::conversation::message::{Message, MessageContent};
use goose_provider_types::conversation::token_usage::ProviderUsage;
use goose_provider_types::conversation::Conversation;
use goose_provider_types::errors::ProviderError;
use goose_provider_types::model::ModelConfig;
use rmcp::model::{CallToolRequestParams, CallToolResult, ErrorData, JsonObject, Tool};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// A host callback must finish only after its effect's durable step did.
#[async_trait]
pub trait Host: Send + Sync {
    async fn model(&self, messages: &[Value]) -> Result<Value>;
    async fn tool(&self, id: &str, name: &str, args: &JsonObject) -> Result<Value>;
    /// Returns queued user messages and whether the turn was stopped.
    async fn commit(&self, event: Value) -> Result<Value>;
}

#[derive(Clone)]
pub struct Session {
    conversation: Conversation,
    wire: Vec<Value>,
}

impl MachineSession for Session {
    fn id(&self) -> &str {
        "turn"
    }
    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conversation)
    }
}

pub enum Effect {
    Message(Message),
    Usage,
}

impl From<Message> for Effect {
    fn from(message: Message) -> Self {
        Self::Message(message)
    }
}

impl InferenceEffect for Effect {
    // The host's paid model step already records and settles usage.
    fn record_usage(_: ProviderUsage) -> Self {
        Self::Usage
    }
}

impl MachineEffect for Effect {
    fn ensure_message_ids(&mut self) {
        // IDs never key effects: the host's Workflow run/step does. Avoid
        // random IDs changing anything when a job body is replayed.
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn user(wire: &Value) -> Message {
    Message::user().with_text(text(&wire["content"]))
}

fn assistant(wire: &Value) -> Result<Message> {
    if wire["tool_calls"]
        .as_array()
        .is_some_and(|calls| calls.len() > 64)
    {
        bail!("one model answer requests at most 64 tools");
    }
    let mut message = Message::assistant().with_text(text(&wire["content"]));
    let mut ids = HashSet::new();
    for tc in wire["tool_calls"].as_array().into_iter().flatten() {
        let id = tc["id"]
            .as_str()
            .ok_or_else(|| anyhow!("a tool call has no id"))?;
        if !ids.insert(id) {
            bail!("duplicate tool call id {id}");
        }
        let name = tc["function"]["name"]
            .as_str()
            .ok_or_else(|| anyhow!("a tool call has no name"))?;
        let raw = &tc["function"]["arguments"];
        let args = match raw {
            Value::String(s) => {
                serde_json::from_str::<JsonObject>(if s.is_empty() { "{}" } else { s })
            }
            _ => serde_json::from_value::<JsonObject>(raw.clone()),
        };
        let call = args
            .map(|args| CallToolRequestParams::new(name.to_owned()).with_arguments(args))
            .map_err(|e| ErrorData::invalid_params(format!("invalid tool arguments: {e}"), None));
        message = message.with_tool_request(id, call);
    }
    if message.as_concat_text().trim().is_empty() && !message.is_tool_call() {
        bail!("the model's last answer was empty");
    }
    Ok(message)
}

struct Model {
    host: Arc<dyn Host>,
    session: Arc<Mutex<Session>>,
    answer: Arc<Mutex<Option<Value>>>,
}

#[async_trait]
impl Provider for Model {
    fn get_name(&self) -> &str {
        "fragment"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        _: &[Message],
        _: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        // Goose controls when inference happens. The platform's adapter
        // transports the exact wire messages so cache marks and opaque
        // thinking are not flattened through a different provider format.
        let wire = self.session.lock().unwrap().wire.clone();
        let answer = self
            .host
            .model(&wire)
            .await
            .map_err(|e| ProviderError::ExecutionError(e.to_string()))?;
        let message = assistant(&answer["message"])
            .map_err(|e| ProviderError::ExecutionError(e.to_string()))?;
        *self.answer.lock().unwrap() = Some(answer);
        Ok(Box::pin(futures::stream::iter([Ok((Some(message), None))])))
    }
}

struct Tools {
    host: Arc<dyn Host>,
    definitions: Vec<Tool>,
}

#[async_trait]
impl ToolProvider<Session> for Tools {
    async fn tools(&self, session: &Session) -> Result<Vec<Tool>> {
        let mut tools = self.definitions.clone();
        // An invented tool is still dispatched to the host's error path;
        // otherwise an unanswered request would reach inference again.
        for request in session
            .conversation
            .messages()
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(MessageContent::as_tool_request)
        {
            if let Ok(call) = &request.tool_call {
                if !tools.iter().any(|t| t.name == call.name) {
                    tools.push(Tool::new(
                        call.name.clone(),
                        "Unavailable tool",
                        Arc::new(JsonObject::new()),
                    ));
                }
            }
        }
        Ok(tools)
    }

    async fn call(
        &self,
        _: &Session,
        id: &str,
        call: CallToolRequestParams,
        _: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        if !self.definitions.iter().any(|tool| tool.name == call.name) {
            return Err(ErrorData::invalid_params(
                format!("no tool named {}", call.name),
                None,
            ));
        }
        let out = self
            .host
            .tool(id, &call.name, &call.arguments.unwrap_or_default())
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::structured(out))
    }
}

struct Store {
    session: Arc<Mutex<Session>>,
    answer: Arc<Mutex<Option<Value>>>,
    host: Arc<dyn Host>,
    cancel: CancellationToken,
    result: Mutex<Value>,
}

#[async_trait]
impl SessionLoader<Session> for Store {
    async fn load(&self, _: &str) -> Result<Session> {
        Ok(self.session.lock().unwrap().clone())
    }
}

#[async_trait]
impl EffectHandler<Session, Effect> for Store {
    async fn apply_effects(
        &self,
        session: &Session,
        effects: &mut [Effect],
        _: &Emitter,
    ) -> Result<()> {
        for effect in effects {
            if let Effect::Message(message) = effect {
                if let Some(error) = message.content.iter().find_map(MessageContent::as_error) {
                    bail!("{}", error.message);
                }
            }
            let (message, wire, event) = match effect {
                Effect::Usage => continue,
                Effect::Message(message) if message.role == rmcp::model::Role::Assistant => {
                    let answer = self
                        .answer
                        .lock()
                        .unwrap()
                        .take()
                        .ok_or_else(|| anyhow!("Goose's inference had no wire answer"))?;
                    (
                        message.clone(),
                        vec![answer["message"].clone()],
                        json!({ "type": "model", "answer": answer }),
                    )
                }
                Effect::Message(message) => {
                    let mut wire = Vec::new();
                    let mut results = Vec::new();
                    for response in message
                        .content
                        .iter()
                        .filter_map(MessageContent::as_tool_response)
                    {
                        let out = match &response.tool_result {
                            Ok(result) => result.structured_content.clone().unwrap_or_else(|| json!({ "text": format!("Error: {}", serde_json::to_string(&result.content).unwrap()) })),
                            Err(error) => json!({ "text": format!("Error: {}", error.message) }),
                        };
                        let echo = out["text"]
                            .as_str()
                            .ok_or_else(|| anyhow!("a tool output has no text"))?;
                        wire.push(
                            json!({ "role": "tool", "tool_call_id": response.id, "content": echo }),
                        );
                        results.push(json!({ "id": response.id, "output": out }));
                    }
                    (
                        message.clone(),
                        wire,
                        json!({ "type": "tools", "results": results }),
                    )
                }
            };
            let receipt = self.host.commit(event).await?;
            let mut next = session.clone();
            next.conversation.push(message);
            next.wire.extend(wire);
            for wire in receipt["messages"].as_array().into_iter().flatten() {
                next.conversation.push(user(wire));
                next.wire.push(wire.clone());
            }
            if serde_json::to_vec(&next.wire)?.len() > 4 * 1024 * 1024 {
                bail!("Goose's turn exceeds 4 MiB of wire messages");
            }
            *self.session.lock().unwrap() = next;
            if receipt["stopped"].as_bool() == Some(true) {
                self.cancel.cancel();
            }
            if let Some(result) = receipt.get("result") {
                *self.result.lock().unwrap() = result.clone();
            }
        }
        Ok(())
    }
}

/// Fresh Goose context per turn; only the supplied shared-memory view is
/// seeded. All effects are replayed through the host's durable steps.
pub async fn run(
    messages: Vec<Value>,
    definitions: Vec<Value>,
    host: Arc<dyn Host>,
) -> Result<Value> {
    if serde_json::to_vec(&messages)?.len() + serde_json::to_vec(&definitions)?.len() > 1024 * 1024
    {
        bail!("Goose's turn input exceeds 1 MiB");
    }
    if messages.is_empty() || messages.len() > 128 {
        bail!("a turn starts with 1–128 messages");
    }
    if definitions.len() > 64 {
        bail!("a turn offers at most 64 tools");
    }
    let conversation =
        Conversation::new_unvalidated(messages.iter().filter(|m| m["role"] == "user").map(user));
    if conversation.is_empty() {
        bail!("a turn needs a user message");
    }
    let definitions = definitions
        .into_iter()
        .map(|wire| {
            let f = &wire["function"];
            let name = f["name"]
                .as_str()
                .ok_or_else(|| anyhow!("a tool has no name"))?;
            let schema: JsonObject = serde_json::from_value(f["parameters"].clone())?;
            Ok(Tool::new(
                name.to_owned(),
                f["description"].as_str().unwrap_or("").to_owned(),
                Arc::new(schema),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let cancel = CancellationToken::new();
    let session = Arc::new(Mutex::new(Session {
        conversation,
        wire: messages,
    }));
    let answer = Arc::new(Mutex::new(None));
    let store = Store {
        session: session.clone(),
        answer: answer.clone(),
        host: host.clone(),
        cancel: cancel.clone(),
        result: Mutex::new(json!({ "state": "done" })),
    };
    let provider = Arc::new(Model {
        host: host.clone(),
        session,
        answer,
    });
    let machine = StateMachine::new(
        vec![
            Step::Operation(Arc::new(ToolOperation::new().with_provider(Arc::new(
                Tools {
                    host: host.clone(),
                    definitions,
                },
            )))),
            Step::Inference(Arc::new(InferenceRunner::new(
                provider,
                ModelConfig::new("fragment"),
            ))),
        ],
        cancel.clone(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    let emit = Emitter::new(tx, cancel);
    // Bound both model and tool phases. The host additionally bounds paid
    // calls, outputs and Workflow steps, forcing a final no-tools answer.
    let drive = async {
        for _ in 0..80 {
            let session = store.load("turn").await?;
            let Some(mut step) = machine.step(&session, &emit).await? else {
                return Ok(store.result.lock().unwrap().clone());
            };
            machine.apply(&store, &session, &mut step, &emit).await?;
        }
        bail!("Goose exceeded 80 state-machine steps")
    };
    let drain = async { while rx.recv().await.is_some() {} };
    // `emit` owns tx: drain until drive finishes, then drop the pending
    // receiver. No runtime or thread is started in the Worker.
    futures::pin_mut!(drive, drain);
    match futures::future::select(drive, drain).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(_) => bail!("Goose's event stream ended before its run"),
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm;
