// vision-mcp: 给无视觉模型加"眼睛"的 MCP server。
// 手写 stdio JSON-RPC (MCP)，图片经 opencode.ai 网关 (mimo-v2.5) 转成文字描述。
use base64::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const MODEL: &str = "mimo-v2.5";
const BASE_URL: &str = "https://opencode.ai/zen/go/v1/chat/completions";
// opencode.ai 在 Cloudflare 后，缺浏览器头会 403 (error code: 1010)
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

        // 无 id 的通知（notifications/initialized 等）直接忽略
        let response = match method {
            "initialize" => Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "vision-mcp", "version": "0.1.0" }
            })),
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({
                "tools": [{
                    "name": "analyze_image",
                    "description": "Analyze an image file with a vision model and return a text description. Pass a local file path in `image`. Optionally give `prompt` to focus the analysis (e.g. 'read the error message', 'what colors are used').",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "image": { "type": "string", "description": "Path to the image file (png/jpg/jpeg/gif/webp)" },
                            "prompt": { "type": "string", "description": "What to focus on; default: describe the image" },
                            "maxTokens": { "type": "integer", "description": "Max output tokens; default 2048" }
                        },
                        "required": ["image"]
                    }
                }]
            })),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if name == "analyze_image" {
                    let result = call_vision(&args);
                    Some(json!({ "content": [{ "type": "text", "text": result }] }))
                } else {
                    Some(json!({ "content": [], "isError": true, "error": format!("unknown tool: {name}") }))
                }
            }
            _ => None, // 未知方法：不回，避免死循环
        };

        if let (Some(id), Some(result)) = (id, response) {
            let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            if writeln!(out, "{}", reply).is_err() {
                break;
            }
            let _ = out.flush();
        }
    }
}

fn call_vision(args: &Value) -> String {
    let path = match args.get("image").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return err_msg("missing required argument: image (usage: {\"image\": \"/path/to.png\", optional \"prompt\", \"maxTokens\"})"),
    };
    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("Describe this image in detail, including any visible text, colors and layout.")
        .to_string();
    let max_tokens = args.get("maxTokens").and_then(|v| v.as_u64()).unwrap_or(4096);

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return err_msg(&format!("cannot read {path}: {e}")),
    };
    let mime = mime_for(path);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    let data_url = format!("data:{mime};base64,{b64}");

    let api_key = match std::env::var("OPENCODE_GO_KEY") {
        Ok(k) => k,
        Err(_) => return err_msg("OPENCODE_GO_KEY not set"),
    };
    let model = std::env::var("VISION_MODEL").unwrap_or_else(|_| MODEL.to_string());

    let body = json!({
        "model": model,
        "reasoning_effort": "none", // mimo-v2.5 默认思考模式会吞掉全部 token，禁用后 content 直出
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }],
        "max_tokens": max_tokens
    });

    let resp = ureq::post(BASE_URL)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", UA)
        .set("Accept", "application/json")
        .set("Accept-Language", "zh-CN,zh;q=0.9")
        .set("Origin", "https://opencode.ai")
        .set("Referer", "https://opencode.ai/")
        .send_string(&body.to_string());

    match resp {
        Ok(r) => {
            let text = r.into_string().unwrap_or_default();
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => v
                    .pointer("/choices/0/message/content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        v.get("error")
                            .map(|e| err_msg(&format!("openrouter error: {e}")))
                            .unwrap_or_else(|| err_msg("no content in response"))
                    }),
                Err(_) => err_msg(&format!("invalid json from openrouter: {}", truncate(&text, 500))),
            }
        }
        Err(e) => match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                err_msg(&format!("openrouter http {code}: {}", truncate(&body, 500)))
            }
            other => err_msg(&format!("request failed: {other}")),
        },
    }
}

fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

fn err_msg(s: &str) -> String {
    format!("ERROR: {s}")
}

fn truncate(s: &str, n: usize) -> String {
    let mut t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        t.push_str("...");
    }
    t
}
