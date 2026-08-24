//! OpenAI-compatible LLM adapter implementing `domain::LlmPort`.
//! v0.1 delivers the trait impl shape only; no network calls/secrets (§5 Out of Scope).

use domain::ports::CompletionChunk;
use std::error::Error;

pub struct OpenAiLlm {
    pub endpoint: String,
    pub model: String,
}

impl OpenAiLlm {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
        }
    }
}

impl domain::LlmPort for OpenAiLlm {
    fn send(
        &mut self,
        _system: &str,
        _prompt: &str,
    ) -> Result<String, Box<dyn Error + Send + Sync>> {
        Err("llm network disabled in v0.1.0".into())
    }

    fn stream<'a>(
        &'a mut self,
        _system: &'a str,
        _prompt: &'a str,
    ) -> Box<dyn Iterator<Item = Result<CompletionChunk, Box<dyn Error + Send + Sync>>> + 'a> {
        Box::new(std::iter::once(Ok(CompletionChunk {
            delta: String::new(),
            done: true,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::LlmPort;

    #[test]
    fn stub_does_not_call_network() {
        let mut llm = OpenAiLlm::new("http://localhost:9999", "gpt-4");
        let res = llm.send("sys", "hi");
        assert!(res.is_err());
    }

    #[test]
    fn stream_yields_single_done_chunk() {
        let mut llm = OpenAiLlm::new("http://localhost:9999", "gpt-4");
        let chunks: Vec<_> = llm.stream("sys", "hi").collect();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].as_ref().unwrap().done);
    }
}
