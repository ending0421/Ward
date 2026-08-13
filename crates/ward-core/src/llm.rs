//! Shared LLM provider plumbing (M2 narration, M4-b intent drift).
//!
//! Providers are optional and configured purely from the environment —
//! `WARD_LLM_URL` (required), optional `WARD_LLM_KEY` / `WARD_LLM_MODEL`.
//! When absent, every LLM-dependent feature degrades to its deterministic
//! fallback (F6 / M4-b partition), never to an error.

use crate::narrate::LlmProvider;

/// Build an OpenAI-compatible HTTP provider from the environment.
pub fn http_llm_from_env() -> Option<Box<dyn LlmProvider>> {
    let url = std::env::var("WARD_LLM_URL").ok()?;
    Some(Box::new(HttpLlm {
        url,
        key: std::env::var("WARD_LLM_KEY").ok(),
        model: std::env::var("WARD_LLM_MODEL").unwrap_or_else(|_| "default".into()),
    }))
}

struct HttpLlm {
    url: String,
    key: Option<String>,
    model: String,
}

impl LlmProvider for HttpLlm {
    fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 400,
            "messages": [{ "role": "user", "content": prompt }],
        });
        let mut req = ureq::post(&self.url).header("Content-Type", "application/json");
        if let Some(key) = &self.key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let mut resp = req.send_json(&body)?;
        let parsed: serde_json::Value = resp.body_mut().read_json()?;
        parsed["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("unexpected LLM response shape"))
    }
}
