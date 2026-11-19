//! Temporary, non-production compatibility spike for the pinned OpenJarvis
//! Rust inference-engine crate. No tools, files, credentials or network calls
//! are exposed or executed by these tests.

use openjarvis_engine::{InferenceEngine, OllamaEngine, OpenAICompatEngine};

/// Construct the candidate local and OpenAI-compatible engines without
/// performing health checks or sending requests.
pub fn candidate_engines() -> Vec<Box<dyn InferenceEngine>> {
    vec![
        Box::new(OllamaEngine::with_defaults()),
        Box::new(OpenAICompatEngine::new(
            "openai",
            "https://api.openai.com",
            Some("test-key-not-used".to_owned()),
            30.0,
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_engine_crate_exposes_local_and_openai_compatible_adapters() {
        let engines = candidate_engines();
        let ids: Vec<&str> = engines.iter().map(|engine| engine.engine_id()).collect();
        assert_eq!(ids, vec!["ollama", "openai"]);
    }

    #[test]
    fn engine_adapters_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OllamaEngine>();
        assert_send_sync::<OpenAICompatEngine>();
    }
}
