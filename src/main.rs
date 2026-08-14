// vision-mcp: 给无视觉模型加"眼睛"的 MCP server。
// 手写 stdio JSON-RPC (MCP)，图片经 opencode.ai 网关 (mimo-v2.5) 转成文字描述。
// P0: 意图传递(结构化输出) + locate 坐标模式 + 本地尺寸上报 + 可操作错误。
// P1: region 裁剪放大通道 + pixel_diff + image_stats（像素层本地计算，零 token）。
use base64::Engine;
use image::imageops;
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
                "serverInfo": { "name": "vision-mcp", "version": "0.3.0" }
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
                            "region": { "type": "string", "description": "Crop 'x1,y1,x2,y2' (original pixel coords) and locally upscale before sending — use for small targets. Coordinates in the reply are converted back to original-image coordinates." },
                            "maxTokens": { "type": "integer", "description": "Max output tokens; default 4096" }
                        },
                        "required": ["image"]
                    }
                },{
                    "name": "image_stats",
                    "description": "Local deterministic image inspection (NO model call, NO tokens): format, dimensions, top-5 dominant colors with hex + share. Use to inspect an image before deciding to analyze it, or to check colors exactly.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "image": { "type": "string", "description": "Path to the image file" }
                        },
                        "required": ["image"]
                    }
                },{
                    "name": "ocr_image",
                    "description": "OCR a (possibly very tall) screenshot locally by chunking: tall images are split into overlapping blocks, each transcribed by the vision model, results merged with block markers. Use for long screenshots / chat logs / scroll captures.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "image": { "type": "string", "description": "Path to the image file" },
                            "maxTokens": { "type": "integer", "description": "Max output tokens per block; default 2048" }
                        },
                        "required": ["image"]
                    }
                },{
                    "name": "trace_svg",
                    "description": "Locally vectorize a flat high-contrast graphic (icon/logo/line art) into black-on-transparent SVG paths: grayscale + threshold + Zhang-Suen center-line thinning + polyline tracing. NO model call, zero tokens, deterministic. Returns SVG markup — write it to a file. Use image_stats first to confirm the source is flat and high-contrast.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "image": { "type": "string", "description": "Path to the image file" },
                            "threshold": { "type": "integer", "description": "Grayscale cutoff (0-255); default 128" }
                        },
                        "required": ["image"]
                    }
                }]
            })),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let result = match name {
                    "analyze_image" => call_vision(&args),
                    "image_stats" => image_stats(&args),
                    "pixel_diff" => pixel_diff(&args),
                    "ocr_image" => ocr_image(&args),
                    "trace_svg" => trace_svg(&args),
                    other => format!("ERROR: unknown tool: {other}"),
                };
                Some(json!({ "content": [{ "type": "text", "text": result }] }))
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
    let max_tokens = args.get("maxTokens").and_then(|v| v.as_u64()).unwrap_or(4096).max(1);

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return err_msg(&format!("cannot read {path}: {e}")),
    };
    // P1: region 裁剪放大通道——小目标先本地放大再送模型，坐标换算回原图（确定性计算，服务端做）
    let mut payload = data;
    let mut mime = mime_for(path);
    let mut region_info: Option<(u32, u32, u32)> = None; // (x1, y1, scale)
    let region_note = match parse_region(args) {
        Some((x1, y1, x2, y2)) => match crop_upscale(&payload, x1, y1, x2, y2) {
            Ok((img, scale)) => {
                mime = "image/png";
                payload = img;
                region_info = Some((x1, y1, scale));
                format!("当前是原图区域 {x1},{y1}-{x2},{y2} 裁剪后放大 {scale}x 的视图（输出坐标使用当前视图坐标，服务端会自动换算回原图）。")
            }
            Err(e) => return err_msg(&e),
        },
        None => String::new(),
    };
    let dims = image_dims(&payload);
    let size_note = dims
        .map(|(w, h)| format!("图片尺寸: {w}x{h} 像素。{region_note}"))
        .unwrap_or_else(|| region_note.clone());

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

    match send_vision(&payload, &mime, &prompt, max_tokens) {
        Ok(s) => {
            if mode == "locate" {
                extract_json_array(&s)
                    .map(|j| convert_bboxes(&j, region_info))
                    .unwrap_or_else(|| format!("WARNING: model output is not a JSON array, returning raw text:\n{s}"))
            } else {
                s
            }
        }
        Err(e) => e,
    }
}

/// 发送一张图 + 提示词到视觉模型，返回文本（locate/ocr 等模式共享）。
fn send_vision(data: &[u8], mime: &str, prompt: &str, max_tokens: u64) -> Result<String, String> {
    let api_key = std::env::var("OPENCODE_GO_KEY").map_err(|_| err_msg("OPENCODE_GO_KEY not set"))?;
    let model = std::env::var("VISION_MODEL").unwrap_or_else(|_| MODEL.to_string());
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let data_url = format!("data:{mime};base64,{b64}");

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
                    .ok_or_else(|| {
                        v.get("error")
                            .map(|e| err_msg(&format!("openrouter error: {e}")))
                            .unwrap_or_else(|| err_msg("no content in response"))
                    }),
                Err(_) => Err(err_msg(&format!(
                    "invalid json from openrouter: {}",
                    truncate(&text, 500)
                ))),
            }
        }
        Err(e) => match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                Err(err_msg(&format!(
                    "openrouter http {code}: {} (hint: {})",
                    truncate(&body, 500),
                    http_hint(code)
                )))
            }
            other => Err(err_msg(&format!("request failed: {other}"))),
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

/// 把 locate 的 bbox 从区域视图坐标换算回原图坐标（确定性，本地做）。
fn convert_bboxes(json: &str, region: Option<(u32, u32, u32)>) -> String {
    let Some((x1, y1, scale)) = region else {
        return json.to_string();
    };
    let Ok(mut v) = serde_json::from_str::<Value>(json) else {
        return json.to_string();
    };
    if let Some(arr) = v.as_array_mut() {
        for item in arr.iter_mut() {
            if let Some(obj) = item.as_object_mut() {
                for key in ["x1", "x2"] {
                    if let Some(n) = obj.get(key).and_then(|n| n.as_f64()) {
                        obj.insert(key.to_string(), json!((x1 as f64 + n / scale as f64).round() as i64));
                    }
                }
                for key in ["y1", "y2"] {
                    if let Some(n) = obj.get(key).and_then(|n| n.as_f64()) {
                        obj.insert(key.to_string(), json!((y1 as f64 + n / scale as f64).round() as i64));
                    }
                }
            }
        }
    }
    v.to_string()
}

/// "x1,y1,x2,y2" → 裁剪区域；非法/缺失返回 None。
fn parse_region(args: &Value) -> Option<(u32, u32, u32, u32)> {
    let s = args.get("region").and_then(|v| v.as_str())?;
    let mut it = s.split(',').filter_map(|t| t.trim().parse::<u32>().ok());
    let (x1, y1) = (it.next()?, it.next()?);
    let (x2, y2) = (it.next()?, it.next()?);
    if x2 <= x1 || y2 <= y1 {
        return None;
    }
    Some((x1, y1, x2, y2))
}

/// 裁剪 region 并整数倍放大（最短边到 ≥512，上限 4x），返回编码后的 PNG + 放大倍数。
fn crop_upscale(data: &[u8], x1: u32, y1: u32, x2: u32, y2: u32) -> Result<(Vec<u8>, u32), String> {
    let img = image::load_from_memory(data).map_err(|_| "cannot decode image for region crop")?;
    let (w, h) = (img.width(), img.height());
    if x1 >= w || y1 >= h {
        return Err(format!("region out of bounds (image is {w}x{h})"));
    }
    let (x1, y1, x2, y2) = (x1.min(w), y1.min(h), x2.min(w), y2.min(h));
    if x2 <= x1 || y2 <= y1 {
        return Err("invalid region (x2<=x1 or y2<=y1)".into());
    }
    let crop = img.crop_imm(x1, y1, x2 - x1, y2 - y1);
    let (cw, ch) = (crop.width(), crop.height());
    let scale = (512 / cw.min(ch).max(1)).max(1).min(4);
    let resized = if scale > 1 {
        imageops::resize(&crop.to_rgba8(), cw * scale, ch * scale, imageops::FilterType::Lanczos3)
    } else {
        crop.to_rgba8()
    };
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(resized)
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|_| "cannot encode cropped image")?;
    Ok((out.into_inner(), scale))
}

/// 本地确定性工具：格式/尺寸/主色（top5 hex+占比），零 token。
fn image_stats(args: &Value) -> String {
    let path = match args.get("image").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return err_msg("missing required argument: image"),
    };
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return err_msg(&format!("cannot read {path}: {e}")),
    };
    let img = match image::load_from_memory(&data) {
        Ok(i) => i,
        Err(e) => return err_msg(&format!("cannot decode {path}: {e}")),
    };
    let fmt = image::guess_format(&data)
        .map(|f| format!("{f:?}"))
        .unwrap_or_else(|_| "unknown".into());
    let small = imageops::resize(&img.to_rgba8(), 32, 32, imageops::FilterType::Triangle);
    let mut buckets: std::collections::HashMap<u16, u32> = std::collections::HashMap::new();
    let mut total = 0u32;
    for p in small.pixels() {
        let key = (((p[0] >> 4) as u16) << 8) | (((p[1] >> 4) as u16) << 4) | ((p[2] >> 4) as u16);
        *buckets.entry(key).or_insert(0) += 1;
        total += 1;
    }
    let mut top: Vec<_> = buckets.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let colors: Vec<String> = top
        .iter()
        .take(5)
        .map(|(k, c)| {
            let (r, g, b) = ((((k >> 8) << 4) + 8) as u8, ((((k >> 4) & 0xF) << 4) + 8) as u8, (((k & 0xF) << 4) + 8) as u8);
            format!("#{r:02X}{g:02X}{b:02X} {:.0}%", *c as f64 * 100.0 / total as f64)
        })
        .collect();
    format!("format: {fmt}\nsize: {}x{}\ncolors: {}", img.width(), img.height(), colors.join(", "))
}

/// 本地确定性 diff：统一尺寸后逐像素比较，输出总差异 + 8x8 网格差异区域，零 token。
fn pixel_diff(args: &Value) -> String {
    let (p1, p2) = match (
        args.get("image1").and_then(|v| v.as_str()),
        args.get("image2").and_then(|v| v.as_str()),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return err_msg("missing required argument: image1, image2"),
    };
    let load = |p: &str| -> Result<image::RgbaImage, String> {
        let data = std::fs::read(p).map_err(|e| format!("cannot read {p}: {e}"))?;
        image::load_from_memory(&data)
            .map(|i| i.to_rgba8())
            .map_err(|e| format!("cannot decode {p}: {e}"))
    };
    let a = match load(p1) {
        Ok(v) => v,
        Err(e) => return err_msg(&e),
    };
    let b = match load(p2) {
        Ok(v) => v,
        Err(e) => return err_msg(&e),
    };
    let (w, h) = (a.width().max(b.width()), a.height().max(b.height()));
    let (wa, ha) = (a.width(), a.height());
    let (wb, hb) = (b.width(), b.height());
    let ra = if wa != w || ha != h {
        imageops::resize(&a, w, h, imageops::FilterType::Triangle)
    } else {
        a
    };
    let rb = if wb != w || hb != h {
        imageops::resize(&b, w, h, imageops::FilterType::Triangle)
    } else {
        b
    };
    let mut diff = 0u64;
    let mut grid = [0u32; 64];
    let n = (w as u64) * (h as u64);
    for y in 0..h {
        for x in 0..w {
            let pa = ra.get_pixel(x, y);
            let pb = rb.get_pixel(x, y);
            let d = u32::from(pa[0]).abs_diff(u32::from(pb[0]))
                + u32::from(pa[1]).abs_diff(u32::from(pb[1]))
                + u32::from(pa[2]).abs_diff(u32::from(pb[2]));
            if d > 60 {
                diff += 1;
                let gx = (x * 8 / w).min(7) as usize;
                let gy = (y * 8 / h).min(7) as usize;
                grid[gy * 8 + gx] += 1;
            }
        }
    }
    let pct = diff as f64 * 100.0 / n.max(1) as f64;
    let gw = (w + 7) / 8;
    let gh = (h + 7) / 8;
    let mut regions = Vec::new();
    let cell_area = ((gw * gh) as f64).max(1.0);
    for gy in 0..8usize {
        for gx in 0..8usize {
            let cell = grid[gy * 8 + gx];
            if cell > 0 {
                let c = cell as f64 * 100.0 / cell_area;
                if c > 10.0 {
                    regions.push(format!(
                        "  grid r{}c{}: {:.0}% of cell differs, pixel x:{}..{} y:{}..{}",
                        gy + 1,
                        gx + 1,
                        c,
                        gx as u32 * gw,
                        (gx as u32 + 1) * gw,
                        gy as u32 * gh,
                        (gy as u32 + 1) * gh
                    ));
                }
            }
        }
    }
    let size_note = if wa != wb || ha != hb {
        format!(" (resized to common {}x{})", w, h)
    } else {
        String::new()
    };
    format!(
        "diff: {:.2}% ({} px of {}){}\nregions (grid 8x8, >10% of cell differs):\n{}",
        pct,
        diff,
        n,
        size_note,
        if regions.is_empty() { "  none".to_string() } else { regions.join("\n") }
    )
}

/// 长截图分块 OCR：高 > 1200px 时自动切成重叠块逐块转录，合并输出。
const OCR_BLOCK_H: u32 = 1200;
const OCR_OVERLAP: u32 = 120;
const OCR_PROMPT: &str = "逐字转录图片中的所有文字，保留原有顺序与分段（说话人、时间戳、引用关系也一并转录）。只输出文字内容本身，不要任何解释。";

fn ocr_image(args: &Value) -> String {
    let path = match args.get("image").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return err_msg("missing required argument: image"),
    };
    let max_tokens = args.get("maxTokens").and_then(|v| v.as_u64()).unwrap_or(2048).max(1);
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return err_msg(&format!("cannot read {path}: {e}")),
    };
    let img = match image::load_from_memory(&data) {
        Ok(i) => i,
        Err(e) => return err_msg(&format!("cannot decode {path}: {e}")),
    };
    let blocks = chunk_blocks(img.height());
    let mut out = Vec::new();
    for (i, (y, bh)) in blocks.iter().enumerate() {
        let block = img.crop_imm(0, *y, img.width(), *bh).to_rgba8();
        let mut buf = std::io::Cursor::new(Vec::new());
        if image::DynamicImage::ImageRgba8(block)
            .write_to(&mut buf, image::ImageFormat::Png)
            .is_err()
        {
            return err_msg("cannot encode block");
        }
        match send_vision(&buf.into_inner(), "image/png", OCR_PROMPT, max_tokens) {
            Ok(t) => out.push(format!("[block {}/{} y:{}..{}]\n{}", i + 1, blocks.len(), y, y + bh, t)),
            Err(e) => out.push(format!("[block {}/{} ERROR]\n{}", i + 1, blocks.len(), e)),
        }
    }
    let note = if blocks.len() > 1 {
        format!("共 {} 块，相邻块 {}px 重叠用于衔接；合并时去重边界重复内容。\n\n", blocks.len(), OCR_OVERLAP)
    } else {
        String::new()
    };
    format!("{note}{}", out.join("\n\n"))
}

/// 水平分块：每块 OCR_BLOCK_H 高，相邻块重叠 OCR_OVERLAP。
fn chunk_blocks(h: u32) -> Vec<(u32, u32)> {
    if h <= OCR_BLOCK_H {
        return vec![(0, h)];
    }
    let mut blocks = Vec::new();
    let mut y = 0u32;
    while y < h {
        let bh = (h - y).min(OCR_BLOCK_H);
        blocks.push((y, bh));
        y += OCR_BLOCK_H - OCR_OVERLAP;
    }
    blocks
}

/// 本地矢量拟合：灰度阈值 + Zhang-Suen 中心线细化 + 折线追踪 → SVG。零模型调用。
fn trace_svg(args: &Value) -> String {
    let path = match args.get("image").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return err_msg("missing required argument: image"),
    };
    let threshold = args.get("threshold").and_then(|v| v.as_u64()).unwrap_or(128) as u8;
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return err_msg(&format!("cannot read {path}: {e}")),
    };
    let img = match image::load_from_memory(&data) {
        Ok(i) => i,
        Err(e) => return err_msg(&format!("cannot decode {path}: {e}")),
    };
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 3 || h < 3 {
        return err_msg("image too small for tracing");
    }
    let gray = img.to_luma8();
    let mut bin = vec![vec![false; w]; h];
    for y in 0..h {
        for x in 0..w {
            bin[y][x] = gray.get_pixel(x as u32, y as u32)[0] < threshold;
        }
    }
    thin_zs(&mut bin, w, h);
    // 实心块 guard：细化后前景仍占比过高 → 不是线稿，拒绝（避免蛇形巨型 path）
    let mut fg = 0usize;
    for row in bin.iter() {
        fg += row.iter().filter(|&&v| v).count();
    }
    let fg_ratio = fg as f64 * 100.0 / (w * h) as f64;
    if fg_ratio > 45.0 {
        return err_msg(&format!("input looks like a solid fill ({fg_ratio:.0}% foreground after thinning), not line art — trace needs flat high-contrast strokes"));
    }
    let paths = trace_paths(&bin, w, h);
    let mut d = String::new();
    for p in &paths {
        if p.len() < 2 {
            continue;
        }
        d.push_str(&format!("M{} {} ", p[0].0, p[0].1));
        for pt in &p[1..] {
            d.push_str(&format!("L{} {} ", pt.0, pt.1));
        }
    }
    if d.is_empty() {
        return "no foreground found (try lower threshold)".to_string();
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\"><path d=\"{}\" stroke=\"black\" stroke-width=\"1\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>",
        w,
        h,
        w,
        h,
        d.trim()
    )
}

/// Zhang-Suen 细化：两步子迭代直到收敛。
fn thin_zs(bin: &mut [Vec<bool>], w: usize, h: usize) {
    let passes: [bool; 2] = [true, false];
    loop {
        let mut changed = false;
        for first in passes {
            let mut to_remove: Vec<(usize, usize)> = Vec::new();
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    if !bin[y][x] {
                        continue;
                    }
                    let (p2, p3, p4, p5, p6, p7, p8, p9) = n8(bin, x, y);
                    let seq = [p2, p3, p4, p5, p6, p7, p8, p9];
                    let b = seq.iter().filter(|&&v| v).count();
                    if b < 2 || b > 6 || transitions(&seq) != 1 {
                        continue;
                    }
                    let removable = if first {
                        !(p2 && p4 && p6) && !(p4 && p6 && p8)
                    } else {
                        !(p2 && p4 && p8) && !(p2 && p6 && p8)
                    };
                    if removable {
                        to_remove.push((x, y));
                    }
                }
            }
            if !to_remove.is_empty() {
                changed = true;
                for (x, y) in to_remove {
                    bin[y][x] = false;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn n8(bin: &[Vec<bool>], x: usize, y: usize) -> (bool, bool, bool, bool, bool, bool, bool, bool) {
    (
        bin[y - 1][x],
        bin[y - 1][x + 1],
        bin[y][x + 1],
        bin[y + 1][x + 1],
        bin[y + 1][x],
        bin[y + 1][x - 1],
        bin[y][x - 1],
        bin[y - 1][x - 1],
    )
}

/// 8 邻域 0→1 转变次数。
fn transitions(seq: &[bool; 8]) -> u32 {
    let mut n = 0;
    for i in 0..8 {
        if !seq[i] && seq[(i + 1) % 8] {
            n += 1;
        }
    }
    n
}

/// 骨架折线化：从端点/叉点追踪，闭合环兜底。
fn trace_paths(bin: &[Vec<bool>], w: usize, h: usize) -> Vec<Vec<(u32, u32)>> {
    let neighbors = |x: usize, y: usize| -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h
                    && bin[ny as usize][nx as usize]
                {
                    v.push((nx as usize, ny as usize));
                }
            }
        }
        v
    };
    let mut visited = vec![vec![false; w]; h];
    let mut paths: Vec<Vec<(u32, u32)>> = Vec::new();
    // 第一遍：从端点（≤1 邻居）起步，遇叉点/已访问停
    let mut starts: Vec<(usize, usize)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if bin[y][x] && !visited[y][x] && neighbors(x, y).len() <= 1 {
                starts.push((x, y));
            }
        }
    }
    for s in starts {
        if visited[s.1][s.0] {
            continue;
        }
        let mut path = Vec::new();
        let (mut cx, mut cy) = s;
        loop {
            if visited[cy][cx] {
                break;
            }
            visited[cy][cx] = true;
            path.push((cx as u32, cy as u32));
            let nbrs: Vec<_> = neighbors(cx, cy).into_iter().filter(|(x, y)| !visited[*y][*x]).collect();
            if nbrs.len() != 1 {
                break;
            }
            (cx, cy) = nbrs[0];
        }
        if path.len() >= 2 {
            paths.push(path);
        }
    }
    // 第二遍：闭合环（无端点）兜底
    for y in 0..h {
        for x in 0..w {
            if !bin[y][x] || visited[y][x] {
                continue;
            }
            let mut path = Vec::new();
            let (mut cx, mut cy) = (x, y);
            loop {
                if visited[cy][cx] {
                    break;
                }
                visited[cy][cx] = true;
                path.push((cx as u32, cy as u32));
                let nbrs: Vec<_> = neighbors(cx, cy).into_iter().filter(|(x, y)| !visited[*y][*x]).collect();
                if nbrs.is_empty() {
                    break;
                }
                (cx, cy) = nbrs[0];
            }
            if path.len() >= 2 {
                paths.push(path);
            }
        }
    }
    paths
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

    #[test]
    fn region_parse() {
        assert_eq!(parse_region(&json!({"region": "10,20,100,200"})), Some((10, 20, 100, 200)));
        assert_eq!(parse_region(&json!({"region": "10,20,5,200"})), None);
        assert_eq!(parse_region(&json!({})), None);
        assert_eq!(parse_region(&json!({"region": "a,b,c,d"})), None);
    }

    #[test]
    fn convert_bboxes_maps_to_original_coords() {
        let j = r#"[{"label":"输入框","x1":0,"y1":70,"x2":100,"y2":100}]"#;
        let out = convert_bboxes(j, Some((40, 20, 4)));
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            serde_json::from_str::<Value>(r#"[{"label":"输入框","x1":40,"y1":38,"x2":65,"y2":45}]"#).unwrap()
        );
        // 无 region：原样
        assert_eq!(convert_bboxes(j, None), j);
    }

    #[test]
    fn convert_bboxes_handles_float_coords() {
        let j = r#"[{"label":"btn","x1":100.0,"y1":1067.5,"x2":200.5,"y2":300}]"#;
        let out = convert_bboxes(j, Some((10, 5, 4)));
        let v = serde_json::from_str::<Value>(&out).unwrap();
        // 10 + 100/4 = 35; 5 + 1067.5/4 = 271.875 → 272; 10 + 200.5/4 = 60; 5 + 300/4 = 80
        assert_eq!(v[0]["x1"], json!(35));
        assert_eq!(v[0]["y1"], json!(272));
        assert_eq!(v[0]["x2"], json!(60));
        assert_eq!(v[0]["y2"], json!(80));
    }

    #[test]
    fn crop_upscale_scales_small_region() {
        let mut img = image::RgbaImage::new(100, 100);
        fill_red(&mut img);
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let (out, scale) = crop_upscale(&buf.into_inner(), 0, 0, 50, 50).unwrap();
        assert_eq!(scale, 4); // 512/50 → cap 4
        assert_eq!(image_dims(&out), Some((200, 200)));
    }

    #[test]
    fn crop_upscale_rejects_out_of_bounds() {
        let mut img = image::RgbaImage::new(100, 100);
        fill_red(&mut img);
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let bytes = buf.into_inner();
        assert!(crop_upscale(&bytes, 5000, 5000, 6000, 6000).is_err());
        assert!(crop_upscale(&bytes, 10, 10, 5, 20).is_err()); // x2 <= x1
    }

    #[test]
    fn pixel_diff_counts_and_grids() {
        let mut a = image::RgbaImage::new(80, 40);
        fill_red(&mut a);
        for y in 0..40 {
            for x in 40..80 {
                a.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(a).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let dir = std::env::temp_dir();
        let p1 = dir.join("vdiff_a.png");
        let p2 = dir.join("vdiff_b.png");
        std::fs::write(&p1, buf.into_inner()).unwrap();
        std::fs::write(&p2, {
            let mut img = image::RgbaImage::new(80, 40);
            fill_red(&mut img);
            let mut b = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(img).write_to(&mut b, image::ImageFormat::Png).unwrap();
            b.into_inner()
        })
        .unwrap();
        let out = pixel_diff(&json!({"image1": p1.to_str().unwrap(), "image2": p2.to_str().unwrap()}));
        assert!(out.contains("50.00%"), "{out}");
        assert!(out.contains("grid r"), "{out}");
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);
    }

    #[test]
    fn image_stats_reports_dimensions_and_colors() {
        let mut img = image::RgbaImage::new(10, 10);
        fill_red(&mut img);
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let dir = std::env::temp_dir();
        let p = dir.join("vstats.png");
        std::fs::write(&p, buf.into_inner()).unwrap();
        let out = image_stats(&json!({"image": p.to_str().unwrap()}));
        assert!(out.contains("10x10"), "{out}");
        assert!(out.contains("#F80808"), "{out}"); // 桶中心色
        let _ = std::fs::remove_file(&p);
    }

    fn fill_red(img: &mut image::RgbaImage) {
        for p in img.pixels_mut() {
            *p = image::Rgba([255u8, 0, 0, 255]);
        }
    }

    #[test]
    fn chunk_blocks_single_for_short() {
        assert_eq!(chunk_blocks(1000), vec![(0, 1000)]);
    }

    #[test]
    fn chunk_blocks_overlap_and_cover() {
        let blocks = chunk_blocks(3000);
        assert_eq!(blocks, vec![(0, 1200), (1080, 1200), (2160, 840)]);
        assert_eq!(blocks[0].0 + blocks[0].1, 1200);
        // 相邻块重叠 = OCR_OVERLAP，且最后一块覆盖到末尾
        for pair in blocks.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + pair[0].1 - OCR_OVERLAP);
        }
        let (last_y, last_h) = *blocks.last().unwrap();
        assert_eq!(last_y + last_h, 3000);
    }

    #[test]
    fn transitions_count() {
        assert_eq!(transitions(&[false, true, true, false, false, false, false, false]), 1);
        assert_eq!(transitions(&[true, false, true, false, true, false, true, false]), 4);
    }

    #[test]
    fn trace_svg_produces_path_for_rect_frame() {
        // 20x20 空心矩形框（1px 边框，x/y 5..14）
        let mut img = image::RgbaImage::new(20, 20);
        for p in img.pixels_mut() {
            *p = image::Rgba([255u8, 255, 255, 255]);
        }
        for i in 5..15 {
            for &(x, y) in &[(i, 5), (i, 14), (5, i), (14, i)] {
                img.put_pixel(x, y, image::Rgba([0u8, 0, 0, 255]));
            }
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let dir = std::env::temp_dir();
        let p = dir.join("vtrace.png");
        std::fs::write(&p, buf.into_inner()).unwrap();
        let out = trace_svg(&json!({"image": p.to_str().unwrap()}));
        assert!(out.starts_with("<svg"), "{out}");
        assert!(out.contains("<path d=\"M"), "{out}");
        assert!(out.contains("viewBox=\"0 0 20 20\""), "{out}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn trace_svg_rejects_solid_fill() {
        let mut img = image::RgbaImage::new(40, 40);
        for p in img.pixels_mut() {
            *p = image::Rgba([0u8, 0, 0, 255]); // 全黑实心
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let dir = std::env::temp_dir();
        let p = dir.join("vsolid.png");
        std::fs::write(&p, buf.into_inner()).unwrap();
        let out = trace_svg(&json!({"image": p.to_str().unwrap()}));
        assert!(out.contains("solid fill"), "{out}");
        assert!(!out.contains("<svg"), "{out}");
        let _ = std::fs::remove_file(&p);
    }
}
