//! `grantAgentAccess`: the phone allows the server's agent to work here for a while, inside named folders (wiqo_core::grants).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use wiqo_core::engine::{Context, Operation, Outcome};

use crate::op::{error, payload, Result};
use crate::Op;

const MAX_MINUTES: u64 = 30;
const MAX_FOLDERS: usize = 4;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(GrantAgentAccess)]
}

/// `AgentGrantPayload`.
#[derive(Deserialize)]
struct Grant {
    minutes: u64,
    #[serde(default)]
    folders: Vec<String>,
    #[serde(default)]
    label: String,
}

struct GrantAgentAccess;

#[async_trait]
impl Operation for GrantAgentAccess {
    fn name(&self) -> &'static str {
        "grantAgentAccess"
    }

    async fn run(&self, ctx: &Context, value: &Value) -> Outcome {
        let request: Result<Grant> = payload(value, "agentGrant", "görev izni");
        let grant = match request {
            Ok(grant)
                if grant.minutes <= MAX_MINUTES
                    && grant.folders.len() <= MAX_FOLDERS
                    && grant.folders.iter().all(|folder| {
                        wiqo_contract::is_valid_local_path(folder) || folder == "~"
                    }) =>
            {
                grant
            }
            _ => {
                return Outcome::failed(error(
                    "INVALID_PAYLOAD",
                    "Görev izni geçersiz.",
                    "tool_failure",
                ))
            }
        };
        let minutes = grant.minutes;
        ctx.services
            .access
            .grant(minutes, grant.folders, grant.label);
        Outcome {
            message: if minutes == 0 {
                "PC görev izni kapatıldı.".into()
            } else {
                format!("PC görev izni verildi ({minutes} dk).")
            },
            data: None,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_grant_is_given_withdrawn_and_bounded() {
        let ctx = Context::for_tests();
        let given = GrantAgentAccess.run(&ctx, &json!({ "kind": "agentGrant", "value": { "minutes": 10, "folders": ["~/Desktop"], "label": "Rapor" } })).await;
        assert!(given.error.is_none());
        assert_eq!(
            ctx.services.access.active().unwrap().folders,
            vec!["~/Desktop".to_string()]
        );
        let too_long = GrantAgentAccess
            .run(
                &ctx,
                &json!({ "kind": "agentGrant", "value": { "minutes": 600, "folders": [] } }),
            )
            .await;
        assert_eq!(too_long.error.unwrap().code, "INVALID_PAYLOAD");
        GrantAgentAccess
            .run(
                &ctx,
                &json!({ "kind": "agentGrant", "value": { "minutes": 0, "folders": [] } }),
            )
            .await;
        assert!(ctx.services.access.active().is_none());
    }
}
