// SPDX-License-Identifier: GPL-2.0-or-later
use serde_json::{Map, Value, json};

pub fn to_chat_request(body: &Value, upstream_model: &str) -> Value {
    let mut out = Map::new();
    out.insert("model".to_owned(), json!(upstream_model));

    let mut messages = Vec::new();
    if let Some(system) = system_text(body.get("system")) {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        push_message(&mut messages, message);
    }
    out.insert("messages".to_owned(), Value::Array(messages));

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool["name"],
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("input_schema").cloned()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                    }
                })
            })
            .collect();
        if !tools.is_empty() {
            out.insert("tools".to_owned(), Value::Array(tools));
            if let Some(choice) = tool_choice(body.get("tool_choice")) {
                out.insert("tool_choice".to_owned(), choice);
            }
        }
    }

    for key in ["max_tokens", "temperature", "top_p", "stream"] {
        if let Some(value) = body.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(stop) = body.get("stop_sequences") {
        out.insert("stop".to_owned(), stop.clone());
    }
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert("stream_options".to_owned(), json!({"include_usage": true}));
    }
    Value::Object(out)
}

fn system_text(system: Option<&Value>) -> Option<String> {
    let text = match system? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn tool_choice(choice: Option<&Value>) -> Option<Value> {
    let choice = choice?;
    match choice.get("type").and_then(Value::as_str)? {
        "auto" => Some(json!("auto")),
        "any" => Some(json!("required")),
        "none" => Some(json!("none")),
        "tool" => Some(json!({"type": "function", "function": {"name": choice["name"]}})),
        _ => None,
    }
}

fn push_message(out: &mut Vec<Value>, message: &Value) {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let blocks = match message.get("content") {
        Some(Value::String(text)) => {
            out.push(json!({"role": role, "content": text}));
            return;
        }
        Some(Value::Array(blocks)) => blocks,
        _ => return,
    };

    if role == "assistant" {
        let mut text = String::new();
        let mut calls = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
                Some("tool_use") => calls.push(json!({
                    "id": block["id"],
                    "type": "function",
                    "function": {
                        "name": block["name"],
                        "arguments": block.get("input").unwrap_or(&json!({})).to_string(),
                    }
                })),
                _ => {}
            }
        }
        let mut msg = json!({"role": "assistant", "content": text});
        if !calls.is_empty() {
            msg["tool_calls"] = Value::Array(calls);
        }
        out.push(msg);
        return;
    }

    let mut parts = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("tool_result") => out.push(json!({
                "role": "tool",
                "tool_call_id": block["tool_use_id"],
                "content": tool_result_text(block.get("content")),
            })),
            Some("text") => parts.push(json!({"type": "text", "text": block["text"]})),
            Some("image") => {
                let source = &block["source"];
                if let (Some(media), Some(data)) = (
                    source.get("media_type").and_then(Value::as_str),
                    source.get("data").and_then(Value::as_str),
                ) {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{media};base64,{data}")}
                    }));
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return;
    }
    if parts.iter().all(|p| p["type"] == "text") {
        let text: Vec<&str> = parts.iter().filter_map(|p| p["text"].as_str()).collect();
        out.push(json!({"role": "user", "content": text.join("\n")}));
    } else {
        out.push(json!({"role": "user", "content": parts}));
    }
}

fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_tools_and_tool_results() {
        let body = json!({
            "model": "x", "max_tokens": 10, "stream": true,
            "system": [{"type": "text", "text": "sys"}],
            "tools": [{"name": "ls", "description": "d", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "any"},
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "ok"},
                    {"type": "tool_use", "id": "t1", "name": "ls", "input": {"a": 1}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "out"}]},
                    {"type": "text", "text": "next"}]}
            ]
        });
        let out = to_chat_request(&body, "m");
        let m = out["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({"role": "system", "content": "sys"}));
        assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(
            m[3],
            json!({"role": "tool", "tool_call_id": "t1", "content": "out"})
        );
        assert_eq!(m[4], json!({"role": "user", "content": "next"}));
        assert_eq!(out["tool_choice"], "required");
        assert_eq!(out["tools"][0]["function"]["name"], "ls");
        assert_eq!(out["stream_options"]["include_usage"], true);
        assert_eq!(out["model"], "m");
    }
}
