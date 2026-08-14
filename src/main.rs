// vision-mcp: 给无视觉模型加"眼睛"的 MCP server。
// 手写 stdio JSON-RPC (MCP)，图片经 opencode.ai 网关 (mimo-v2.5) 转成文字描述。
// P0: 意图传递(结构化输出) + locate 坐标模式 + 本地尺寸上报 + 可操作错误。
use base64::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const MODEL: &str = "mimo-v2.5";
const BASE_URL: &str = "https://opencode.ai/zen/go/v1/chat/completions";
// opencode.ai 在 Cloudflare 后，缺浏览器头会 403 (error code: 1010)
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

const DEFAULT_PROMPT: &str = "用结构化分段描述这张图片：\n\
## 文字（所有可见文字，逐字给出）\n\
## 布局（主要区域及其大致像素坐标 x1,y1,x2,y2）\n\
## 颜色（主色调）\n\
## 元素（对象/UI 元素清单，尽量给大致边界框坐标）\n\
## 注意（异常、错误、可疑之处）\n\
要具体、如实，能说坐标就不说含糊词。";

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
                "serverInfo": { "name": "vision-mcp", "version": "0.2.0" }
            })),
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({
                "tools": [{
                    "name": "analyze_image",
                    "description": "Analyze an image file with a vision model. Pass a local file path in `image`. ",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "image": { "type": "string", "description": "Path to the image file (png/jpg/jpeg/gif/webp)" },
                            "prompt": { "type": "string", "description": "What to focus on (e.g. 'read the error message', 'what is the main color'). Omit for a structured default report: verbatim text / layout / colors / elements, with pixel coordinates." },
                            "mode": { "type": "string", "enum": ["describe", "locate"], "description": "'locate' returns ONLY a JSON array of bounding boxes [{\"label\",\"x1\",\"y1\",\"x2\",\"y2\"}] in original pixel coordinates — pair with `prompt` naming the target (e.g. 'the send button', 'all buttons'). 'describe' (default) returns a structured text report." },
                            "maxTokens": { "type": "integer", "description": "Max output tokens; default 4096" }
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
        None => return err_msg("missing required argument: image (usage: {\"image\": \"/path/to.png\", optional \"prompt\", \"mode\", \"maxTokens\"})"),
    };
    let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("describe");
    let user_prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let max_tokens = args.get("maxTokens").and_then(|v| v.as_u64()).unwrap_or(4096);

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return err_msg(&format!("cannot read {path}: {e}")),
    };
    let dims = image_dims(&data);
    let size_note = dims
        .map(|(w, h)| format!("图片尺寸: {w}x{h} 像素。"))
        .unwrap_or_default();

    // 意图传递：尺寸信息喂给模型（模型据此判断长截图/小图标），模式决定提示词形态
    let prompt = match mode {
        "locate" => {
            let target = if user_prompt.is_empty() { "图中所有主要 UI 元素/对象" } else { user_prompt };
            format!(
                "{size_note}在图片中定位目标元素。只输出一个 JSON 数组，不要任何其他文字或代码块标记：\n\
                 [{{\"label\": \"元素名\", \"x1\": 0, \"y1\": 0, \"x2\": 0, \"y2\": 0}}]\n\
                 坐标使用原图像素坐标。目标：{target}"
            )
        }
        _ if user_prompt.is_empty() => format!("{size_note}{DEFAULT_PROMPT}"),
        _ => format!("{size_note}用户关注点：{user_prompt}\n直接回答，尽量具体；涉及位置时用原图像素坐标 (x1,y1,x2,y2)。"),
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
                    .map(|s| {
                        if mode == "locate" {
                            extract_json_array(s)
                                .unwrap_or_else(|| format!("WARNING: model output is not a JSON array, returning raw text:\n{s}"))
                        } else {
                            s.to_string()
                        }
                    })
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
                err_msg(&format!(
                    "openrouter http {code}: {} (hint: {})",
                    truncate(&body, 500),
                    http_hint(code)
                ))
            }
            other => err_msg(&format!("request failed: {other}")),
        },
    }
}

/// 从模型输出里提取第一个 JSON 数组（locate 模式），避免散文/代码块污染。
fn extract_json_array(text: &str) -> Option<String> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end <= start {
        return None;
    }
    let slice = &text[start..=end];
    if serde_json::from_str::<Value>(slice).is_ok() {
        Some(slice.to_string())
    } else {
        None
    }
}

/// 本地解析图片尺寸（PNG/GIF/JPEG 头，零依赖），像素层信息不下放给模型。
fn image_dims(b: &[u8]) -> Option<(u32, u32)> {
    // PNG: magic + IHDR，宽高为 big-endian u32，位于 16/20
    if b.len() >= 24 && b[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some((be32(&b[16..20]), be32(&b[20..24])));
    }
    // GIF: "GIF87a"/"GIF89a"，宽高为 little-endian u16，位于 6/8
    if b.len() >= 10 && (&b[..6] == b"GIF87a" || &b[..6] == b"GIF89a") {
        return Some((u32::from(le16(&b[6..8])), u32::from(le16(&b[8..10]))));
    }
    // JPEG: 扫描 marker 找 SOF（C0-CF，除 C4/C8/CC），高/宽位于段内 +5/+7
    if b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 {
        let mut i = 2usize;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xD8 || marker == 0x01 {
                i += 2;
                continue;
            }
            if marker == 0xD9 {
                break;
            }
            if i + 4 > b.len() {
                break;
            }
            let seg_len = be16(&b[i + 2..i + 4]) as usize;
            let is_sof = matches!(
                marker,
                0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
            );
            if is_sof && i + 9 <= b.len() {
                return Some((u32::from(be16(&b[i + 7..i + 9])), u32::from(be16(&b[i + 5..i + 7]))));
            }
            if seg_len < 2 {
                break;
            }
            i += 2 + seg_len;
        }
    }
    None
}

fn http_hint(code: u16) -> &'static str {
    match code {
        401 | 403 => "check OPENCODE_GO_KEY",
        413 => "image too large — resize or crop it first",
        429 => "rate limited — retry in a moment",
        400 => "bad request — try reducing maxTokens or image size",
        _ => "see response body",
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

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes()); // IHDR chunk length
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v
    }

    #[test]
    fn dims_png() {
        assert_eq!(image_dims(&png(1920, 1080)), Some((1920, 1080)));
    }

    #[test]
    fn dims_gif() {
        let mut v = b"GIF89a".to_vec();
        v.extend_from_slice(&320u16.to_le_bytes());
        v.extend_from_slice(&240u16.to_le_bytes());
        assert_eq!(image_dims(&v), Some((320, 240)));
    }

    #[test]
    fn dims_jpeg_skips_appn() {
        // SOI + APP0(16B) + SOF0(11B, 300x600) + EOI
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(b"JFIF\0\x01\x01\x00\x00\x01\x00\x01\x00\x00");
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&300u16.to_be_bytes());
        v.extend_from_slice(&600u16.to_be_bytes());
        v.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        assert_eq!(image_dims(&v), Some((600, 300)));
    }

    #[test]
    fn dims_unknown() {
        assert_eq!(image_dims(b"not an image"), None);
    }

    #[test]
    fn extract_array_handles_fence_and_prose() {
        let out = "好的，以下是结果：\n```json\n[{\"label\":\"按钮\",\"x1\":1,\"y1\":2,\"x2\":3,\"y2\":4}]\n```\n以上。";
        assert_eq!(
            extract_json_array(out).unwrap(),
            r#"[{"label":"按钮","x1":1,"y1":2,"x2":3,"y2":4}]"#
        );
    }

    #[test]
    fn extract_array_rejects_invalid() {
        assert_eq!(extract_json_array("没有数组"), None);
        assert_eq!(extract_json_array("[not json]"), None);
    }
}
