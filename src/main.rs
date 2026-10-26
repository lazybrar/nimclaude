// SPDX-License-Identifier: GPL-2.0-or-later

mod log;
mod request;
mod response;

use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, IsTerminal},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

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

const DEFAULT_MODEL: &str = "moonshotai/kimi-k2.6";

struct Config {
    api_key: String,
    chat_url: String,
    model_override: Option<String>,
    proxy_key: Option<String>,
    http: reqwest::Client,
}

fn config_path() -> PathBuf {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("nimclaude").join("config")
}

fn read_settings() -> BTreeMap<String, String> {
    let text = fs::read_to_string(config_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

fn write_settings(settings: &BTreeMap<String, String>) -> io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text: String = settings.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    fs::write(&path, text)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
}

fn setting(settings: &BTreeMap<String, String>, env_name: &str, key: &str) -> Option<String> {
    env::var(env_name)
        .ok()
        .or_else(|| settings.get(key).cloned())
        .filter(|v| !v.is_empty())
}

fn fail(message: &str) -> ! {
    eprintln!("nimclaude: {message}");
    std::process::exit(1)
}

const USAGE: &str = "usage:
  nimclaude set api <NVIDIA_API_KEY>   save your key (once)
  nimclaude set model <model-id>       choose the model (default: moonshotai/kimi-k2.6)
  nimclaude show                       print the saved settings
  nimclaude logs [path]                print the last 60 log lines (or the log file path)
  nimclaude [claude args...]           start the proxy and launch claude through it
  nimclaude serve                      only run the proxy (HOST/PORT, default 127.0.0.1:4001)";

fn set_command(args: &[String]) {
    let [what, value] = args else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let key = match what.as_str() {
        "api" => "api",
        "model" => "model",
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let mut settings = read_settings();
    settings.insert(key.to_owned(), value.clone());
    if let Err(err) = write_settings(&settings) {
        fail(&format!("cannot write {}: {err}", config_path().display()));
    }
    println!("saved {key} to {}", config_path().display());
}

fn logs_command(arg: Option<&str>) {
    let path = log::path();
    if arg == Some("path") {
        println!("{}", path.display());
        return;
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    println!("# {}", path.display());
    for line in &lines[lines.len().saturating_sub(60)..] {
        println!("{line}");
    }
}

fn show_command() {
    let settings = read_settings();
    let api = settings.get("api").map_or("(not set)".to_owned(), |k| {
        format!("{}...", k.chars().take(8).collect::<String>())
    });
    let model = settings.get("model").map_or(DEFAULT_MODEL, String::as_str);
    println!(
        "config: {}\napi:    {api}\nmodel:  {model}",
        config_path().display()
    );
}

fn prompt_api_key(settings: &mut BTreeMap<String, String>) -> String {
    if !io::stdin().is_terminal() {
        fail("no API key yet. run: nimclaude set api <NVIDIA_API_KEY>");
    }
    eprint!("first run. paste your NVIDIA API key (nvapi-...): ");
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() || line.trim().is_empty() {
        fail("no key entered");
    }
    let key = line.trim().to_owned();
    settings.insert("api".to_owned(), key.clone());
    if let Err(err) = write_settings(settings) {
        fail(&format!("cannot write {}: {err}", config_path().display()));
    }
    eprintln!("saved to {}", config_path().display());
    key
}

fn load_config() -> Arc<Config> {
    let mut settings = read_settings();
    let api_key = match setting(&settings, "NVIDIA_API_KEY", "api") {
        Some(key) => key,
        None => prompt_api_key(&mut settings),
    };
    let base = env::var("NIMCLAUDE_BASE")
        .unwrap_or_else(|_| "https://integrate.api.nvidia.com".to_owned());
    Arc::new(Config {
        api_key,
        chat_url: format!("{}/v1/chat/completions", base.trim_end_matches('/')),
        model_override: Some(
            setting(&settings, "NIMCLAUDE_MODEL", "model")
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        ),
        proxy_key: env::var("NIMCLAUDE_KEY").ok().filter(|k| !k.is_empty()),
        http: reqwest::Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .expect("http client"),
    })
}

fn router(config: Arc<Config>) -> Router {
    Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/health", get(|| async { "ok" }))
        .with_state(config)
}

async fn serve_command() {
    log::echo_to_stderr();
    let config = load_config();
    let host = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = env::var("PORT").unwrap_or_else(|_| "4001".to_owned());
    let listener = match tokio::net::TcpListener::bind(format!("{host}:{port}")).await {
        Ok(l) => l,
        Err(err) => fail(&format!("cannot bind {host}:{port}: {err}")),
    };
    log::log(&format!(
        "serve start: http://{host}:{port} model={}",
        config.model_override.as_deref().unwrap_or("-")
    ));
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let _ = axum::serve(listener, router(config))
        .with_graceful_shutdown(shutdown)
        .await;
}

async fn run_command(claude_args: &[String]) {
    let config = load_config();
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(err) => fail(&format!("cannot bind a local port: {err}")),
    };
    let addr = listener
        .local_addr()
        .expect("bound listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(config)).await;
    });
    let status = tokio::process::Command::new("claude")
        .args(claude_args)
        .env("ANTHROPIC_BASE_URL", format!("http://{addr}"))
        .env("ANTHROPIC_AUTH_TOKEN", "nimclaude")
        .env_remove("ANTHROPIC_API_KEY")
        .status()
        .await;
    match status {
        Ok(status) => {
            log::log(&format!("session end: claude exited with {status}"));
            std::process::exit(status.code().unwrap_or(1))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => fail(
            "claude is not installed or not in PATH. install Claude Code first (https://claude.com/claude-code)",
        ),
        Err(err) => fail(&format!("cannot start claude: {err}")),
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-V" | "--version") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
        }
        Some("-h" | "--help" | "help") => println!("{USAGE}"),
        Some("set") => set_command(&args[1..]),
        Some("show") => show_command(),
        Some("logs") => logs_command(args.get(1).map(String::as_str)),
        Some("serve") => serve_command().await,
        _ => run_command(&args).await,
    }
}

fn chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        out.push_str(": ");
        out.push_str(&inner.to_string());
        source = inner.source();
    }
    out
}

fn error(status: StatusCode, message: &str) -> Response {
    let body = json!({"type": "error", "error": {"type": "api_error", "message": message}});
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
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

async fn count_tokens(
    State(config): State<Arc<Config>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&config, &headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let tokens = body.len() / 4;
    axum::Json(json!({"input_tokens": tokens})).into_response()
}

struct StreamGuard {
    id: u64,
    start: Instant,
    bytes: usize,
    done: bool,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let how = if self.done {
            "finished"
        } else {
            "client closed early"
        };
        log::log(&format!(
            "#{} response {how}: {} upstream bytes in {} ms",
            self.id,
            self.bytes,
            self.start.elapsed().as_millis()
        ));
    }
}

fn fail_response(id: u64, code: StatusCode, message: &str) -> Response {
    log::log(&format!(
        "#{id} error {}: {}",
        code.as_u16(),
        log::clip(message, 600)
    ));
    error(code, message)
}

async fn messages(State(config): State<Arc<Config>>, headers: HeaderMap, body: Bytes) -> Response {
    let id = log::next_id();
    if !authorized(&config, &headers) {
        return fail_response(id, StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return fail_response(id, StatusCode::BAD_REQUEST, "invalid json");
    };
    let Some(requested) = body.get("model").and_then(Value::as_str) else {
        return fail_response(id, StatusCode::BAD_REQUEST, "missing model");
    };
    let upstream_model = config.model_override.clone().unwrap_or_else(|| {
        requested
            .strip_prefix("nvidia/")
            .unwrap_or(requested)
            .to_owned()
    });
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let chat = request::to_chat_request(&body, &upstream_model);
    let count = |key: &str| body.get(key).and_then(Value::as_array).map_or(0, Vec::len);
    log::log(&format!(
        "#{id} request client_model={requested} -> {upstream_model} stream={stream} messages={} tools={} max_tokens={}",
        count("messages"),
        count("tools"),
        body.get("max_tokens").and_then(Value::as_u64).unwrap_or(0)
    ));
    if log::debug() {
        log::log(&format!(
            "#{id} chat request: {}",
            log::clip(&chat.to_string(), 4000)
        ));
    }

    let start = Instant::now();
    let upstream = match config
        .http
        .post(&config.chat_url)
        .bearer_auth(&config.api_key)
        .json(&chat)
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return fail_response(
                id,
                StatusCode::BAD_GATEWAY,
                &format!("upstream: {}", chain(&err)),
            );
        }
    };

    let status = upstream.status();
    log::log(&format!(
        "#{id} upstream status {status} after {} ms",
        start.elapsed().as_millis()
    ));
    if !status.is_success() {
        let text = upstream.text().await.unwrap_or_default();
        let code = match status.as_u16() {
            404 | 410 => StatusCode::BAD_REQUEST,
            other => StatusCode::from_u16(other).unwrap_or(StatusCode::BAD_GATEWAY),
        };
        return fail_response(
            id,
            code,
            &format!("nvidia {status} (model {upstream_model}): {text}"),
        );
    }

    let translator = response::ChatToMessages::new(requested, stream);
    let guard = StreamGuard {
        id,
        start,
        bytes: 0,
        done: false,
    };
    let chunks = stream::unfold(
        (Box::pin(upstream.bytes_stream()), Some((translator, guard))),
        |(mut chunks, state)| async move {
            let (mut translator, mut guard) = state?;
            match chunks.next().await {
                Some(Ok(chunk)) => {
                    guard.bytes += chunk.len();
                    if log::debug() {
                        let text = String::from_utf8_lossy(&chunk);
                        log::log(&format!("#{} chunk: {}", guard.id, log::clip(&text, 400)));
                    }
                    let out = translator.push(&chunk);
                    Some((Ok(Bytes::from(out)), (chunks, Some((translator, guard)))))
                }
                Some(Err(err)) => {
                    log::log(&format!(
                        "#{} upstream stream error: {}",
                        guard.id,
                        chain(&err)
                    ));
                    Some((Err(io::Error::other(err)), (chunks, None)))
                }
                None => {
                    guard.done = true;
                    let out = translator.finish();
                    Some((Ok(Bytes::from(out)), (chunks, None)))
                }
            }
        },
    );
    let content_type = if stream {
        "text/event-stream"
    } else {
        "application/json"
    };
    let mut resp = Response::new(Body::from_stream(chunks));
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    resp
}
