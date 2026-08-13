//! End-to-end stdio roundtrip: drive the ward-mcp binary with the official
//! MCP Rust SDK client and exercise the real tools.

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::{transport::TokioChildProcess, ServiceExt};
use tokio::process::Command;

fn result_text(res: CallToolResult) -> String {
    res.content
        .into_iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn lists_and_calls_tools_over_stdio() -> anyhow::Result<()> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ward-mcp"));
    // Test cwd is crates/ward-mcp; run the daemon at the workspace root.
    cmd.current_dir("..");
    let client = ().serve(TokioChildProcess::new(cmd)?).await?;

    let tools = client.list_tools(None).await?;
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in ["spot", "replay", "catch_run", "form_check", "spot_action"] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}; got {names:?}"
        );
    }

    // catch_run works without any index — pure inner-loop precheck.
    let res = client
        .call_tool(
            CallToolRequestParams::new("catch_run")
                .with_arguments(serde_json::json!({"repo": ".."}).as_object().cloned().unwrap()),
        )
        .await?;
    let text = result_text(res);
    assert!(
        text.contains("pass") || text.contains("fail") || text.contains("unknown") || text.contains("deferred"),
        "unexpected catch_run payload: {text}"
    );

    // spot with an empty index must still answer (fail-open, no matches).
    let res = client
        .call_tool(
            CallToolRequestParams::new("spot").with_arguments(
                serde_json::json!({
                    "intent": "防抖函数",
                    "proposed_signature": "pub fn debounce(f: &dyn Fn(u64), ms: u64) -> u8",
                    "repo": ".."
                })
                .as_object()
                .cloned()
                .unwrap(),
            ),
        )
        .await?;
    let text = result_text(res);
    assert!(text.contains("\"ok\""), "spot must return the tool envelope: {text}");

    Ok(())
}
