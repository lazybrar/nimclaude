// SPDX-License-Identifier: GPL-2.0-or-later
use serde_json::{Value, json};

#[derive(PartialEq)]
enum Open {
    None,
    Text,
    Tool(u64),
}

pub struct ChatToMessages {
    model: String,
    stream: bool,
    buf: Vec<u8>,
    started: bool,
    done: bool,
    open: Open,
    next_index: u64,
    finish_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    id: String,
}

impl ChatToMessages {
    pub fn new(model: &str, stream: bool) -> Self {
        Self {
            model: model.to_owned(),
            stream,
            buf: Vec::new(),
            started: false,
            done: false,
            open: Open::None,
            next_index: 0,
            finish_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            id: String::new(),
        }
    }

    fn event(out: &mut Vec<u8>, name: &str, data: Value) {
        out.extend_from_slice(format!("event: {name}\ndata: {data}\n\n").as_bytes());
    }

    fn start(&mut self, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        Self::event(
            out,
            "message_start",
            json!({"type": "message_start", "message": {
                "id": self.id, "type": "message", "role": "assistant", "model": self.model,
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": self.input_tokens, "output_tokens": 0}}}),
        );
    }

    fn close_block(&mut self, out: &mut Vec<u8>) {
        if self.open != Open::None {
            Self::event(
                out,
                "content_block_stop",
                json!({"type": "content_block_stop", "index": self.next_index - 1}),
            );
            self.open = Open::None;
        }
    }

    fn open_block(&mut self, out: &mut Vec<u8>, kind: Open, block: Value) {
        self.close_block(out);
        Self::event(
            out,
            "content_block_start",
            json!({"type": "content_block_start", "index": self.next_index, "content_block": block}),
        );
        self.next_index += 1;
        self.open = kind;
    }

    fn delta(&mut self, out: &mut Vec<u8>, delta: Value) {
        Self::event(
            out,
            "content_block_delta",
            json!({"type": "content_block_delta", "index": self.next_index - 1, "delta": delta}),
        );
    }

    fn chunk(&mut self, out: &mut Vec<u8>, chunk: &Value) {
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            if self.id.is_empty() {
                self.id = id.to_owned();
            }
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            self.input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(self.input_tokens);
            self.output_tokens = usage["completion_tokens"]
                .as_u64()
                .unwrap_or(self.output_tokens);
        }
        self.start(out);
        let Some(choice) = chunk["choices"].get(0) else {
            return;
        };
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            if self.open != Open::Text {
                self.open_block(out, Open::Text, json!({"type": "text", "text": ""}));
            }
            self.delta(out, json!({"type": "text_delta", "text": text}));
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = call["index"].as_u64().unwrap_or(0);
            let function = &call["function"];
            if self.open != Open::Tool(index) || call["id"].is_string() {
                self.open_block(
                    out,
                    Open::Tool(index),
                    json!({"type": "tool_use",
                        "id": call["id"].as_str().unwrap_or("call_0"),
                        "name": function["name"].as_str().unwrap_or_default(),
                        "input": {}}),
                );
            }
            if let Some(args) = function["arguments"].as_str().filter(|a| !a.is_empty()) {
                self.delta(
                    out,
                    json!({"type": "input_json_delta", "partial_json": args}),
                );
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_owned());
        }
    }

    fn line(&mut self, out: &mut Vec<u8>, line: &str) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data == "[DONE]" {
            self.end(out);
        } else if let Ok(chunk) = serde_json::from_str::<Value>(data) {
            self.chunk(out, &chunk);
        }
    }

    fn end(&mut self, out: &mut Vec<u8>) {
        if self.done {
            return;
        }
        self.done = true;
        self.start(out);
        self.close_block(out);
        Self::event(
            out,
            "message_delta",
            json!({"type": "message_delta",
                "delta": {"stop_reason": stop_reason(self.finish_reason.as_deref()), "stop_sequence": null},
                "usage": {"output_tokens": self.output_tokens}}),
        );
        Self::event(out, "message_stop", json!({"type": "message_stop"}));
    }

    fn whole_message(&self, body: &Value) -> Value {
        let choice = &body["choices"][0];
        let message = &choice["message"];
        let mut content = Vec::new();
        if let Some(text) = message["content"].as_str().filter(|t| !t.is_empty()) {
            content.push(json!({"type": "text", "text": text}));
        }
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            let input: Value = call["function"]["arguments"]
                .as_str()
                .and_then(|a| serde_json::from_str(a).ok())
                .unwrap_or_else(|| json!({}));
            content.push(json!({"type": "tool_use", "id": call["id"],
                "name": call["function"]["name"], "input": input}));
        }
        json!({
            "id": body["id"], "type": "message", "role": "assistant", "model": self.model,
            "content": content,
            "stop_reason": stop_reason(choice["finish_reason"].as_str()),
            "stop_sequence": null,
            "usage": {"input_tokens": body["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                      "output_tokens": body["usage"]["completion_tokens"].as_u64().unwrap_or(0)}
        })
    }
}

fn stop_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls" | "function_call") => "tool_use",
        _ => "end_turn",
    }
}

impl ChatToMessages {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        if !self.stream {
            return out;
        }
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line).trim_end().to_owned();
            self.line(&mut out, &line);
        }
        out
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.stream {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            self.line(&mut out, rest.trim());
            self.end(&mut out);
        } else if let Ok(body) = serde_json::from_slice::<Value>(&self.buf) {
            out = self.whole_message(&body).to_string().into_bytes();
        } else {
            out = std::mem::take(&mut self.buf);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(stream: bool, input: &str) -> String {
        let mut t = ChatToMessages::new("m", stream);
        let mut out = t.push(input.as_bytes());
        out.extend(t.finish());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn streams_text_and_tool_call() {
        let input = concat!(
            "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"t1\",\"function\":{\"name\":\"ls\",\"arguments\":\"{\\\"a\\\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        );
        let out = run(true, input);
        assert!(out.starts_with("event: message_start"));
        assert!(out.contains("\"text\":\"hi\""));
        assert!(out.contains("\"type\":\"tool_use\""));
        assert!(out.contains("\"partial_json\":\":1}\""));
        assert!(out.contains("\"stop_reason\":\"tool_use\""));
        assert!(out.contains("\"output_tokens\":7"));
        assert_eq!(out.matches("message_stop").count(), 2);
        assert_eq!(out.matches("content_block_stop").count(), 4);
    }

    #[test]
    fn converts_whole_message() {
        let input = r#"{"id":"c1","choices":[{"finish_reason":"stop","message":{"content":"yo","tool_calls":[{"id":"t","function":{"name":"f","arguments":"{\"x\":1}"}}]}}],"usage":{"prompt_tokens":1,"completion_tokens":2}}"#;
        let v: Value = serde_json::from_str(&run(false, input)).unwrap();
        assert_eq!(v["content"][0]["text"], "yo");
        assert_eq!(v["content"][1]["input"]["x"], 1);
        assert_eq!(v["usage"]["output_tokens"], 2);
    }
}
