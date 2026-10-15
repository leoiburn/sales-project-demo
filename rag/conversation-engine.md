---
feature: conversation-engine
files: [crates/automotrix/src/engine.rs, crates/automotrix/src/llm.rs]
status: committed
last_reviewed: 2026-10-15
---

# Conversation engine (one chat turn)

## What it does

`engine::turn(app, convo, text)` takes one customer message and returns a `Reply`: the text, the
vehicles to show as cards, a handoff flag, `bot_paused`, and an optional booking form.

## How it works

1. **Record.** Append the customer message to `messages` and bump `last_message_at`.
2. **Paused?** If `conversations.bot_paused` is true, a human has taken over. The message is stored
   and the bot returns an empty reply with `bot_paused: true`.
3. **Language.** Use the customer's saved `preferred_language`. If there isn't one, use
   `detect_language()`, a keyword check for Spanish markers ("hola", "busco", "¿", "ñ"...). A wrong
   guess costs little: the model answers in whatever language the customer writes in.
4. **System prompt.** Built per turn with the dealer's name, address, phone, today's date in the
   dealer timezone, and the rules: never state facts from memory, use `search_inventory` for cars and
   `search_policies` for policies, repeat `must_include_verbatim` text word for word, prefer the
   booking form, record SMS consent only on an explicit yes, ask once for email consent when given an email and
   record it only on an explicit yes, call `request_human` when stuck, never
   promise approval, rates, trade values or discounts. Style rules: short, like texting, no markdown,
   at most three cars per reply, end with one next step.
5. **History.** `history()` rebuilds the conversation from the transcript. Only the words are kept:
   past tool calls and results are not replayed, to keep requests small. Consecutive messages from
   the same role are merged, because the API wants alternating roles.
6. **Tool loop.** Up to `MAX_TOOL_ROUNDS = 8`: call Claude with `max_tokens` 1500. If
   `stop_reason == "tool_use"`, run every tool it asked for through `tools::dispatch` and send **all**
   results back in one user message (splitting them teaches the model to stop making parallel calls).
   A tool that throws becomes an `is_error` result saying "internal error, apologize and offer a
   salesperson". Shown vehicle ids, the handoff flag and the form are collected along the way.
7. **Fallback.** If the loop ends without text, the reply is "Let me get someone from our team..."
   (or the Spanish version).
8. **Guardrail.** Run the price guardrail on the final text. If it finds violations, log a
   `reply_unbacked_figure` event and ask the model once to rewrite without those figures, with no
   tools and `max_tokens` 800. If the rewrite still fails or is empty, send a fixed "I'd rather have
   our team confirm exact figures" message instead. See [price-guardrail.md](price-guardrail.md).
9. **Disclosure.** On the conversation's first assistant reply, code puts the dealer's AI disclosure
   in front (`dealer_settings.ai_disclosure_en/es`). Code does it, so the model can't forget or
   reword it.
10. **Store.** Append the assistant message with the tool-call log (`tool_calls`, which records name,
    input and is_error) and the `vehicle_ids` shown. Those ids are later the proof of which cars were
    really shown.

## The LLM client (`llm.rs`)

- Raw HTTP to `POST https://api.anthropic.com/v1/messages`, `anthropic-version: 2023-06-01`, with a
  120 s timeout. Rust has no official SDK.
- `Block` covers `text`, `tool_use` and `tool_result`. Any other block type (thinking, citations)
  becomes `Unknown`, so it never crashes parsing.
- Tool definitions carry `strict: true`, so the arguments are guaranteed to match the schema.
- `complete_json()` asks for structured output via `output_config.format`. On any 400 it retries
  without it and parses leniently (`parse_json_lenient` takes the outermost `{...}`). Callers always
  validate the result.
- `Client::configured()` checks that `ANTHROPIC_API_KEY` is set and not blank.

## Configuration

`ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL` (default `claude-haiku-4-5`). The prompt text lives in
`system_prompt()` in `engine.rs`.

## Tests

`detects_spanish_openers`, `history_merges_consecutive_turns` (engine). `parses_fenced_json`,
`tool_result_block_round_trips`, `unknown_blocks_do_not_break_parsing` (llm).

## Gotchas

- Tool results are not replayed into later turns, so the model doesn't remember last turn's raw
  search results, only what it said about them. It has to search again.
- The 400-fallback in `complete_json` matches the string "400" in the error message.

## Related

[tools.md](tools.md), [price-guardrail.md](price-guardrail.md), [leads.md](leads.md)
