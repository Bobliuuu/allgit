//! `vcs-assist-openai`: the same assistant protocol as `vcs-assist-claude`, answered by an OpenAI-compatible
//! chat-completions endpoint. Prompt, extraction and validation are the shared ones: file contents are untrusted
//! data and a proposal is only ever printed, never applied. This crate adds the HTTP client, a model default aimed at
//! cheap models, and a hard per-run budget on requests and tokens.

pub mod api;
