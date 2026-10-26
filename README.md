# nimclaude

Local proxy that lets Claude Code use NVIDIA NIM models (chat-completions API,
e.g. the free models on build.nvidia.com). It accepts Anthropic `/v1/messages`
requests (text, tools, streaming) and translates them to and from
`https://integrate.api.nvidia.com/v1/chat/completions`.

## Setup

1. Install the package: `cd pkg && makepkg -f`, then `sudo pacman -U nimclaude-*.pkg.tar.zst`.
2. Create the env file:

       mkdir -p ~/.config/nimclaude
       cp /usr/share/doc/nimclaude/env.example ~/.config/nimclaude/env
       chmod 600 ~/.config/nimclaude/env

   Set `NVIDIA_API_KEY` and `NIMCLAUDE_MODEL` (any model id from build.nvidia.com).
3. Start it: `systemctl --user enable --now nimclaude`
   (logs: `journalctl --user -u nimclaude -f`). Without systemd, export the
   variables and run `nimclaude`.
4. Run Claude Code against it (fish):

       set -x ANTHROPIC_BASE_URL http://127.0.0.1:4001
       set -x ANTHROPIC_AUTH_TOKEN local
       claude

   `NIMCLAUDE_MODEL` forces every request, including Claude Code's small
   background-model calls, onto that one model. Leave it unset to pass
   `ANTHROPIC_MODEL` through (a leading `nvidia/` is stripped).

## Environment

| variable | default | meaning |
|---|---|---|
| `NVIDIA_API_KEY` | required | NVIDIA API key |
| `NIMCLAUDE_MODEL` | none | force this upstream model for all requests |
| `NIMCLAUDE_KEY` | none | require this token from the client |
| `NIMCLAUDE_BASE` | `https://integrate.api.nvidia.com` | upstream base URL |
| `HOST` / `PORT` | `127.0.0.1` / `4001` | listen address |

## Limits

Images are passed as data URIs, thinking blocks are dropped, and
`count_tokens` is a rough estimate. Free tiers are rate-limited, and tool
calling quality depends on the model (Qwen3-Coder and Llama 3.x instruct are
the safer picks).

## License

GPL-2.0-or-later. See `LICENSE`.
