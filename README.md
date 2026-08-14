# vision-mcp

给无视觉模型加"眼睛"的 MCP server。手写 stdio JSON-RPC，图片经 opencode.ai 网关 (mimo-v2.5) 转成文字描述。单二进制，无本地模型依赖。

## 构建

```bash
cargo build --release
# 产物: target/release/vision-mcp
```

## 环境变量

| 变量 | 必需 | 说明 |
|------|------|------|
| `OPENCODE_GO_KEY` | 是 | opencode.ai Go 订阅的 API key |
| `VISION_MODEL` | 否 | 模型名，默认 `mimo-v2.5` |

## pi 配置

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

`OPENCODE_GO_KEY` 通过 pi 的 `env` 字段或 shell 环境注入，不要写进 mcp.json 提交到仓库。

## 调用

工具名 `vision-mcp_analyze_image`，参数名是 `image`（本地绝对路径）：

```json
{ "image": "/tmp/screenshot.png", "prompt": "read the error message" }
```

不传 `image` 会报 `missing required argument: image`，错误信息自带 usage 提示。

## Agent skill

`skill/SKILL.md` 是给 agent 的识图技能：agent 遇到"用户粘贴图片路径"场景会自动加载，第一次调用就传对参数。复制到你的 skills 目录（如 `~/.agents/skills/vision/SKILL.md`）即可。
