---
name: vision
description: 当用户粘贴图片路径（如 /tmp/pi-clipboard-*.png、截图、或任何本地图片文件）并要求查看、描述、识别、提取图中文字/错误信息时使用。调用 vision-mcp_analyze_image 让无视觉模型获得"眼睛"。也适用于用户直接发图片但当前模型不支持图像输入的场景。
---

# 图片识别（vision-mcp）

## 调用方式

用 MCP 工具 `vision-mcp_analyze_image`，**参数名是 `image`**（不是 path）：

```
mcp call vision-mcp_analyze_image {"image": "/tmp/xxx.png"}
```

| 参数 | 必填 | 说明 |
|------|------|------|
| `image` | ✅ | 图片**本地绝对路径**（png/jpg/jpeg/gif/webp） |
| `prompt` | ❌ | 聚焦点，如 "read the error message"、"what colors are used" |
| `maxTokens` | ❌ | 输出上限，默认 4096 |

## 坑

- 参数名是 `image`——传 `path` 或不传会报 `missing required argument: image`（服务器在 `call_vision` 里直接拒绝，不是网关问题）
- 路径必须是服务器可读的本地路径，如 `/tmp/pi-clipboard-*.png`；不是 URL
- 一次只分析一张图，多张图分开调用
- 报错 `cannot read <path>` = 路径不存在或权限不足
