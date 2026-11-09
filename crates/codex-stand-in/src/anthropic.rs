//! Deterministic Messages API responses for the real Claude client.
use serde_json::{Value, json};

pub fn events(request: &Value, calls: &[(String, Value)]) -> Vec<Value> {
    let completed = request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|block| block["type"] == "tool_result")
        .count();
    let call = calls.get(completed);
    let (block, delta, reason) = match call {
        Some((name, args)) => (
            json!({"type":"tool_use","id":format!("call_{completed}"),"name":name,"input":{}}),
            json!({"type":"input_json_delta","partial_json":args.to_string()}),
            "tool_use",
        ),
        None => (
            json!({"type":"text","text":""}),
            json!({"type":"text_delta","text":"done"}),
            "end_turn",
        ),
    };
    vec![
        json!({"type":"message_start","message":{"id":format!("message_{completed}"),"type":"message","role":"assistant","model":request["model"],"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":block}),
        json!({"type":"content_block_delta","index":0,"delta":delta}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn messages_replay_bash_then_mcp_then_finish() {
        let calls = vec![
            ("Bash".into(), json!({"command":"capabilities"})),
            ("mcp__reach__ping".into(), json!({})),
        ];
        let mut request = json!({"model":"stand-in","messages":[]});
        let first = events(&request, &calls);
        assert_eq!(first[1]["content_block"]["name"], "Bash");
        assert_eq!(first[4]["delta"]["stop_reason"], "tool_use");
        request["messages"] = json!([{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_0","content":"ok"}]}]);
        assert_eq!(
            events(&request, &calls)[1]["content_block"]["name"],
            "mcp__reach__ping"
        );
        request["messages"][0]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"tool_result","tool_use_id":"call_1","content":"reachable"}));
        let last = events(&request, &calls);
        assert_eq!(last[2]["delta"]["text"], "done");
        assert_eq!(last[4]["delta"]["stop_reason"], "end_turn");
    }
}
