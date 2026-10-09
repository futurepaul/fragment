use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use fragment_goose::{run, Host};
use rmcp::model::JsonObject;
use serde_json::{json, Value};

struct Script {
    answers: Mutex<VecDeque<Value>>,
    calls: Mutex<Vec<Vec<Value>>>,
    tools: Mutex<Vec<String>>,
    events: Mutex<Vec<Value>>,
    steer: Mutex<Option<Value>>,
    stop: bool,
}
impl Script {
    fn new(answers: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            calls: Mutex::new(vec![]),
            tools: Mutex::new(vec![]),
            events: Mutex::new(vec![]),
            steer: Mutex::new(None),
            stop: false,
        })
    }
}
#[async_trait]
impl Host for Script {
    async fn model(&self, messages: &[Value]) -> Result<Value> {
        self.calls.lock().unwrap().push(messages.to_vec());
        Ok(
            json!({ "message": self.answers.lock().unwrap().pop_front().expect("unexpected inference") }),
        )
    }
    async fn tool(&self, _: &str, name: &str, _: &JsonObject) -> Result<Value> {
        self.tools.lock().unwrap().push(name.to_owned());
        Ok(
            json!({ "text": if name == "read" { "found" } else { "Error: unavailable tool" }, "task": "task-1" }),
        )
    }
    async fn commit(&self, event: Value) -> Result<Value> {
        let tool_phase = event["type"] == "tools";
        self.events.lock().unwrap().push(event);
        if tool_phase {
            let messages = self
                .steer
                .lock()
                .unwrap()
                .take()
                .into_iter()
                .collect::<Vec<_>>();
            Ok(
                json!({ "messages": messages, "stopped": self.stop, "result": { "state": if self.stop { "stopped" } else { "done" } } }),
            )
        } else {
            Ok(json!({}))
        }
    }
}
fn seed() -> Vec<Value> {
    vec![
        json!({ "role": "system", "content": "fixed prompt" }),
        json!({ "role": "user", "content": [{ "type": "text", "text": "0+1|memory", "cache": "blocks" }, { "type": "text", "text": "question" }] }),
    ]
}
fn definitions() -> Vec<Value> {
    vec![
        json!({ "type": "function", "function": { "name": "read", "description": "Read", "parameters": { "type": "object" } } }),
    ]
}
fn task(name: &str, args: &str) -> Value {
    json!({ "role": "assistant", "content": "Looking", "tool_calls": [{ "id": "call-1", "type": "function", "function": { "name": name, "arguments": args } }], "thinking_blocks": [{ "type": "thinking", "signature": "opaque", "thinking": "" }] })
}
fn answer() -> Value {
    json!({ "role": "assistant", "content": "Done" })
}

#[tokio::test]
async fn answer_ends_without_another_inference() {
    let host = Script::new(vec![answer()]);
    assert_eq!(
        run(seed(), definitions(), host.clone()).await.unwrap(),
        json!({ "state": "done" })
    );
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    assert!(host.tools.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tool_results_and_steering_preserve_the_wire_prefix() {
    let request = task("read", "{}");
    let host = Script::new(vec![request.clone(), answer()]);
    *host.steer.lock().unwrap() = Some(json!({ "role": "user", "content": "also this" }));
    run(seed(), definitions(), host.clone()).await.unwrap();
    let calls = host.calls.lock().unwrap();
    assert_eq!(&calls[1][..2], seed());
    assert_eq!(calls[1][2], request); // opaque thinking blocks unchanged
    assert_eq!(
        calls[1][3],
        json!({ "role": "tool", "tool_call_id": "call-1", "content": "found" })
    );
    assert_eq!(calls[1][4]["content"], "also this");
    assert_eq!(*host.tools.lock().unwrap(), vec!["read"]);
    assert_eq!(
        host.events.lock().unwrap()[1]["results"][0]["output"]["task"],
        "task-1"
    );
}

#[tokio::test]
async fn malformed_arguments_are_answered_without_running_a_tool() {
    let host = Script::new(vec![task("read", "not json"), answer()]);
    run(seed(), definitions(), host.clone()).await.unwrap();
    assert!(host.tools.lock().unwrap().is_empty());
    assert!(host.calls.lock().unwrap()[1][3]["content"]
        .as_str()
        .unwrap()
        .contains("invalid tool arguments"));
}

#[tokio::test]
async fn invented_tool_is_answered_and_does_not_loop() {
    let host = Script::new(vec![task("invented", "{}"), answer()]);
    run(seed(), definitions(), host.clone()).await.unwrap();
    assert!(host.tools.lock().unwrap().is_empty());
    assert!(host.calls.lock().unwrap()[1][3]["content"]
        .as_str()
        .unwrap()
        .contains("no tool named invented"));
    assert_eq!(host.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn stop_after_tools_prevents_the_next_model_call() {
    let host = Arc::new(Script {
        stop: true,
        ..Arc::try_unwrap(Script::new(vec![task("read", "{}")]))
            .ok()
            .unwrap()
    });
    assert_eq!(
        run(seed(), definitions(), host.clone()).await.unwrap()["state"],
        "stopped"
    );
    assert_eq!(host.calls.lock().unwrap().len(), 1);
}

// A minimal durable-step host: a round killed after a tool's effect
// rebuilds the loop from its seed, reading the same recorded answers.
struct Replay {
    script: Arc<Script>,
    cursor: Mutex<usize>,
    records: Mutex<Vec<(Value, Value)>>,
    cut: Mutex<bool>,
}
impl Replay {
    async fn step(&self, input: Value) -> Result<Value> {
        let index = {
            let mut cursor = self.cursor.lock().unwrap();
            let index = *cursor;
            *cursor += 1;
            index
        };
        if let Some((previous, result)) = self.records.lock().unwrap().get(index).cloned() {
            assert_eq!(input, previous, "replay must ask for the same effect");
            return Ok(result);
        }
        if input["kind"] == "commit" && input["event"]["type"] == "tools" {
            let mut cut = self.cut.lock().unwrap();
            if *cut {
                *cut = false;
                anyhow::bail!("simulated eviction after tool effect");
            }
        }
        let result = match input["kind"].as_str().unwrap() {
            "model" => {
                self.script
                    .model(&serde_json::from_value::<Vec<Value>>(
                        input["messages"].clone(),
                    )?)
                    .await?
            }
            "tool" => {
                self.script
                    .tool(
                        input["id"].as_str().unwrap(),
                        input["name"].as_str().unwrap(),
                        &serde_json::from_value(input["args"].clone())?,
                    )
                    .await?
            }
            "commit" => self.script.commit(input["event"].clone()).await?,
            _ => unreachable!(),
        };
        assert_eq!(self.records.lock().unwrap().len(), index);
        self.records.lock().unwrap().push((input, result.clone()));
        Ok(result)
    }
}
#[async_trait]
impl Host for Replay {
    async fn model(&self, messages: &[Value]) -> Result<Value> {
        self.step(json!({ "kind": "model", "messages": messages }))
            .await
    }
    async fn tool(&self, id: &str, name: &str, args: &JsonObject) -> Result<Value> {
        self.step(json!({ "kind": "tool", "id": id, "name": name, "args": args }))
            .await
    }
    async fn commit(&self, event: Value) -> Result<Value> {
        self.step(json!({ "kind": "commit", "event": event })).await
    }
}

#[tokio::test]
async fn restart_replays_recorded_effects_without_repeating_the_tool() {
    let host = Arc::new(Replay {
        script: Script::new(vec![task("read", "{}"), answer()]),
        cursor: Mutex::new(0),
        records: Mutex::new(vec![]),
        cut: Mutex::new(true),
    });
    let error = run(seed(), definitions(), host.clone()).await.unwrap_err();
    assert!(error.to_string().contains("simulated eviction"));
    *host.cursor.lock().unwrap() = 0;
    assert_eq!(
        run(seed(), definitions(), host.clone()).await.unwrap()["state"],
        "done"
    );
    assert_eq!(*host.script.tools.lock().unwrap(), vec!["read"]);
    assert_eq!(host.script.calls.lock().unwrap().len(), 2);
}
