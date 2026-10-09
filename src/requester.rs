use crate::model::{Agent, Caller, valid_session_id};
use rmcp::ErrorData;
use rmcp::handler::server::common::FromContextPart;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::JsonObject;

const SESSION_META: &[(&str, Agent)] = &[("ai.opencode/sessionID", Agent::OpenCode)];

#[derive(Clone, Debug, Default)]
pub struct Requester(pub Option<Caller>);

impl Requester {
    pub fn from_meta(meta: &JsonObject) -> Self {
        Self(SESSION_META.iter().find_map(|(key, agent)| {
            let session_id = meta.get(*key)?.as_str()?;
            valid_session_id(*agent, session_id).then(|| Caller {
                agent: Some(*agent),
                session_id: Some(session_id.to_string()),
            })
        }))
    }
}

impl<S> FromContextPart<ToolCallContext<'_, S>> for Requester {
    fn from_context_part(context: &mut ToolCallContext<'_, S>) -> Result<Self, ErrorData> {
        Ok(Self::from_meta(&context.request_context.meta))
    }
}

#[cfg(test)]
mod tests {
    use super::Requester;
    use crate::model::Agent;
    use serde_json::json;

    fn requester(meta: serde_json::Value) -> Requester {
        Requester::from_meta(meta.as_object().unwrap())
    }

    #[test]
    fn reads_opencode_session_from_request_meta() {
        let caller = requester(json!({ "ai.opencode/sessionID": "ses_abc123" }))
            .0
            .unwrap();
        assert_eq!(caller.agent, Some(Agent::OpenCode));
        assert_eq!(caller.session_id.as_deref(), Some("ses_abc123"));
    }

    #[test]
    fn ignores_missing_invalid_and_foreign_meta() {
        assert!(requester(json!({})).0.is_none());
        assert!(
            requester(json!({ "ai.opencode/sessionID": "../escape" }))
                .0
                .is_none()
        );
        assert!(requester(json!({ "ai.opencode/sessionID": 7 })).0.is_none());
        assert!(
            requester(json!({ "progressToken": "ses_abc123" }))
                .0
                .is_none()
        );
    }
}
