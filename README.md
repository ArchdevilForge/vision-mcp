# vision-mcp

给无视觉模型加"眼睛"的 MCP server。手写 stdio JSON-RPC，图片经 opencode.ai 网关 (mimo-v2.5) 转成文字描述。无需 GPU / 本地模型，零依赖部署（单二进制）。

## 功能

- 输入本地图片路径，返回文字描述（可见文本、颜色、布局等）
- 可选 `prompt` 聚焦分析（如 "read the error message"）
- 可选 `maxTokens` 控制输出上限

## 构建

```bash
cargo build --release
# 产物: target/release/vision-mcp
```

## 环境变量

| 变量 | 必填 | 说明 |
|------|------|------|
| `OPENCODE_GO_KEY` | ✅ | opencode.ai Go 订阅的 API key（Bearer 认证） |
| `VISION_MODEL` | ❌ | 模型名，默认 `mimo-v2.5` |

## pi (coding agent) 配置

在 `~/.pi/agent/mcp.json` 的 `mcpServers` 中加入：

```json
{
  "mcpServers": {
    "vision-mcp": {
      "command": "/absolute/path/to/vision-mcp/target/release/vision-mcp",
      "lifecycle": "eager"
    }
  }
}
```

`OPENCODE_GO_KEY` 通过 pi 的 `env` 字段或 shell 环境注入（不要写进 mcp.json 提交到仓库）。

## 调用

工具名 `vision-mcp_analyze_image`，参数名是 `image`（本地绝对路径），不是 `path`：

```json
{ "image": "/tmp/screenshot.png", "prompt": "read the error message" }
```

不传 `image` 会报 `missing required argument: image`，错误信息自带 usage 提示。

## Agent skill（可选）

仓库内 `skill/SKILL.md` 是给 agent 的识图技能：agent 遇到"用户粘贴图片路径/截图"场景会自动加载，从而第一次调用就传对参数，不再盲调报错。复制到你的 skills 目录（如 `~/.agents/skills/vision/SKILL.md`）即可。
