/// Planner error types.

#[derive(Debug, thiserror::Error)]
pub enum PlannerError {
    #[error("LLM error: {0}")]
    Llm(#[from] cel_llm::LlmError),

    #[error("Failed to parse LLM output after {attempts} attempts: {last_output}")]
    ParseFailed { attempts: u32, last_output: String },

    #[error("Max steps ({max_steps}) exceeded without achieving goal")]
    MaxStepsExceeded { max_steps: u32 },

    #[error("Context provider error: {0}")]
    Context(String),
}
