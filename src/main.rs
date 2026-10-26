// SPDX-License-Identifier: GPL-2.0-or-later

mod request;
mod response;

use std::{env, io, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};

struct Config {
    api_key: String,
    chat_url: String,
    model_override: Option<String>,
    proxy_key: Option<String>,
    http: reqwest::Client,
}

#[tokio::main]
async fn main() {
    if matches!(env::args().nth(1).as_deref(), Some("-V" | "--version")) {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return;
    }
    if env::args().nth(1).is_some() {
        eprintln!("usage: nimclaude   (configured through environment, see README)");
        std::process::exit(2);
    }
    let Some(api_key) = env::var("NVIDIA_API_KEY").ok().filter(|k| !k.is_empty()) else {
        eprintln!("nimclaude: NVIDIA_API_KEY is not set");
        std::process::exit(1);
    };
    let base = env::var("NIMCLAUDE_BASE").unwrap_or_else(|_| "https://integrate.api.nvidia.com".to_owned());
    let config = Arc::new(Config {
        api_key,
        chat_url: format!("{}/v1/chat/completions", base.trim_end_matches('/')),
        model_override: env::var("NIMCLAUDE_MODEL").ok().filter(|m| !m.is_empty()),
        proxy_key: env::var("NIMCLAUDE_KEY").ok().filter(|k| !k.is_empty()),
        http: reqwest::Client::new(),
    });

    let host = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = env::var("PORT").unwrap_or_else(|_| "4001".to_owned());
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/health", get(|| async { "ok" }))
        .with_state(config);
    let listener = match tokio::net::TcpListener::bind(format!("{host}:{port}")).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("nimclaude: cannot bind {host}:{port}: {err}");
            std::process::exit(1);
        }
    };
    eprintln!("nimclaude: listening on http://{host}:{port}");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let _ = axum::serve(listener, app).with_graceful_shutdown(shutdown).await;
}

fn error(status: StatusCode, message: &str) -> Response {
    let body = json!({"type": "error", "error": {"type": "api_error", "message": message}});
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

fn authorized(config: &Config, headers: &HeaderMap) -> bool {
    let Some(key) = &config.proxy_key else {
        return true;
    };
    let given = headers
        .get("x-api-key")
        .or_else(|| headers.get(header::AUTHORIZATION))
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim_start_matches("Bearer "));
    given == Some(key.as_str())
}

async fn count_tokens(State(config): State<Arc<Config>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&config, &headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let tokens = body.len() / 4;
    axum::Json(json!({"input_tokens": tokens})).into_response()
}

async fn messages(State(config): State<Arc<Config>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&config, &headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid json");
    };
    let Some(requested) = body.get("model").and_then(Value::as_str) else {
        return error(StatusCode::BAD_REQUEST, "missing model");
    };
    let upstream_model = config.model_override.clone().unwrap_or_else(|| {
        requested.strip_prefix("nvidia/").unwrap_or(requested).to_owned()
    });
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let chat = request::to_chat_request(&body, &upstream_model);

    let upstream = match config
        .http
        .post(&config.chat_url)
        .bearer_auth(&config.api_key)
        .json(&chat)
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => return error(StatusCode::BAD_GATEWAY, &format!("upstream: {err}")),
    };

    let status = upstream.status();
    if !status.is_success() {
        let text = upstream.text().await.unwrap_or_default();
        let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return error(code, &format!("nvidia {status}: {text}"));
    }

    let translator = response::ChatToMessages::new(requested, stream);
    let chunks = stream::unfold(
        (Box::pin(upstream.bytes_stream()), Some(translator)),
        |(mut chunks, translator)| async move {
            let mut translator = translator?;
            match chunks.next().await {
                Some(Ok(chunk)) => {
                    let out = translator.push(&chunk);
                    Some((Ok(Bytes::from(out)), (chunks, Some(translator))))
                }
                Some(Err(err)) => Some((Err(io::Error::other(err)), (chunks, None))),
                None => {
                    let out = translator.finish();
                    Some((Ok(Bytes::from(out)), (chunks, None)))
                }
            }
        },
    );
    let content_type = if stream { "text/event-stream" } else { "application/json" };
    let mut resp = Response::new(Body::from_stream(chunks));
    resp.headers_mut().insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    resp
}
