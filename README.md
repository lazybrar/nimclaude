# nimclaude

Local proxy that lets Claude Code use NVIDIA NIM models (chat-completions API,
e.g. the free models on build.nvidia.com). It accepts Anthropic `/v1/messages`
requests (text, tools, streaming) and translates them to and from
`https://integrate.api.nvidia.com/v1/chat/completions`.

## Use

    nimclaude set api nvapi-...      # once (or skip: the first run asks for it)
    nimclaude                        # starts the proxy and launches claude through it

Arguments after `nimclaude` go straight to `claude` (`nimclaude --resume`).
The proxy runs on a random local port for the life of that session, and your
normal Claude login is untouched when you run plain `claude`.

| command | meaning |
|---|---|
| `nimclaude set api <key>` | save the NVIDIA API key (`~/.config/nimclaude/config`, mode 600) |
| `nimclaude set model <id>` | choose the model, default `moonshotai/kimi-k2.6` |
| `nimclaude show` | print the saved settings (key shortened) |
| `nimclaude logs [path]` | last 60 log lines, or just the log path (`~/.local/state/nimclaude/nimclaude.log`, rotated at 5 MB) |
| `nimclaude serve` | only run the proxy on `127.0.0.1:4001` (`HOST`/`PORT`), e.g. `systemctl --user enable --now nimclaude` |

The chosen model is used for every request, including Claude Code's small
background-model calls. `NVIDIA_API_KEY`, `NIMCLAUDE_MODEL`, `NIMCLAUDE_KEY`
(require a token from the client) and `NIMCLAUDE_BASE` override the config.
`NIMCLAUDE_MAX_TOKENS` (default 16384) caps the output length Claude Code asks
for, and `NIMCLAUDE_TIMEOUT` (default 120 s) is how long to wait for NVIDIA's first byte.

## Logs

Every request is logged (client model, upstream model, message and tool counts,
upstream status and latency, bytes streamed, errors with the full cause). API
keys are never logged. For troubleshooting, `NIMCLAUDE_LOG=debug nimclaude`
also logs the translated request and the raw upstream chunks (prompts included,
so keep that log private).

## Install

    cd pkg && makepkg -f && sudo pacman -U nimclaude-*.pkg.tar.zst

## Limits

Images are passed as data URIs, thinking blocks are dropped, and
`count_tokens` is a rough estimate. Free tiers are rate-limited, and tool
calling quality depends on the model (Qwen3-Coder and Llama 3.x instruct are
the safer picks).

## License

GPL-2.0-or-later. See `LICENSE`.
