//! Redact prompt content with a custom governance hook.
//!
//! Run with: `cargo run -p cel-brief --example governance`

use async_trait::async_trait;
use cel_brief::{
    Brief, BriefBuilder, BriefContext, BriefMessage, Governance, GovernanceError,
    GovernanceVerdict, RedactionRecord, Role, SystemPromptSource, TokenBudget, UserMessageSource,
};

struct SecretRedactor;

#[async_trait]
impl Governance for SecretRedactor {
    async fn review(
        &self,
        draft: &mut Brief,
        _ctx: &BriefContext,
    ) -> Result<GovernanceVerdict, GovernanceError> {
        let mut redactions = Vec::new();

        for message in &mut draft.messages {
            if let BriefMessage::Text {
                role: Role::User,
                content,
                source,
            } = message
            {
                if content.contains("api_key=") {
                    *content = content.replace("api_key=secret", "api_key=[REDACTED]");
                    redactions.push(RedactionRecord {
                        source: source.clone(),
                        rule: "example:no_api_keys".into(),
                    });
                }
            }
        }

        if redactions.is_empty() {
            Ok(GovernanceVerdict::Allow)
        } else {
            Ok(GovernanceVerdict::Redacted(redactions))
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = BriefContext::new(TokenBudget::default())
        .with_user_message("Deploy is failing with api_key=secret in the log output");

    let brief = BriefBuilder::new()
        .source(std::sync::Arc::new(SystemPromptSource::new(
            "Answer using only the provided sources.",
        )))
        .source(std::sync::Arc::new(UserMessageSource::new()))
        .governance(std::sync::Arc::new(SecretRedactor))
        .build(&ctx)
        .await?;

    println!("messages: {:#?}", brief.messages);
    println!("redactions: {:#?}", brief.receipt.redactions);
    Ok(())
}
