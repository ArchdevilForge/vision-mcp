---
name: vision
description: 当用户粘贴图片路径（如 /tmp/pi-clipboard-*.png、截图或任何本地图片文件）并要求查看、描述、识别、提取图中文字、对比图片、定位元素、还原图标/UI 时使用。调用 vision-mcp_* 工具让无视觉模型获得"眼睛"。也适用于用户直接发图片但当前模型不支持图像输入的场景。
---

# 图片识别（vision-mcp）

5 个工具，按任务选型。参数名是 `image`（本地绝对路径），不是 `path`。

## 工具总览

| 工具 | 何时用 | 成本 |
|------|--------|------|
| `analyze_image` | 看图、读错误信息、定位元素（默认形态） | 模型调用 |
| `image_stats` | 只要尺寸/格式/主色；先探图再决定下一步 | 零 token，本地 |
| `pixel_diff` | 两张截图对比（UI 验收、回归检查） | 零 token，本地 |
| `ocr_image` | 长截图/聊天记录/滚动页面逐字提取文字 | 模型调用（自动分块） |
| `trace_svg` | 图标/Logo/线稿 → 可编辑 SVG | 零 token，本地 |

## analyze_image（核心）

```
mcp call vision-mcp_analyze_image {"image": "/tmp/xxx.png"}
```

| 参数 | 必需 | 说明 |
|------|------|------|
| `image` | 是 | 图片本地绝对路径（png/jpg/jpeg/gif/webp） |
| `prompt` | 否 | 聚焦点，如 "read the error message"、"what is the main color"；省略则输出结构化报告（文字/布局/颜色/元素+像素坐标） |
| `mode` | 否 | `describe`（默认）或 `locate` |
| `region` | 否 | `"x1,y1,x2,y2"` 本地裁剪放大再识别，小目标用；返回坐标已换算回原图 |
| `maxTokens` | 否 | 输出上限，默认 4096 |

**mode: locate** —— 输出纯 JSON bbox 数组 `[{"label","x1","y1","x2","y2"}]`，原图像素坐标。GUI 自动化/UI 还原/找按钮输入框用这个：

```
mcp call vision-mcp_analyze_image {"image": "/tmp/shot.png", "mode": "locate", "prompt": "发送按钮和输入框"}
```

**region** —— 小图标/小字看不清时，先裁出来放大再看（本地放大，目标识别质量质变）：

```
mcp call vision-mcp_analyze_image {"image": "/tmp/shot.png", "mode": "locate", "region": "1000,500,1200,700", "prompt": "这个区域里的按钮"}
```

## 长截图 OCR

```
mcp call vision-mcp_ocr_image {"image": "/tmp/chat-long.png"}
```

- 高 >1200px 自动切成重叠块逐块转录，结果带 `[block n/m]` 标记
- 合并时去重相邻块重叠区的重复内容

## 截图对比（UI 验收闭环）

```
mcp call vision-mcp_pixel_diff {"image1": "/tmp/before.png", "image2": "/tmp/after.png"}
```

- 返回总差异 % + 8x8 网格差异区域（含像素范围）——改 UI 前后各截一张，对比验证

## 图标 → SVG

```
mcp call vision-mcp_trace_svg {"image": "/tmp/icon.png"}
```

- 只适合**扁平高对比**图形（图标/线稿）；先 `image_stats` 确认源图，复杂照片类不适合
- 输出是笔画中心线（骨架），还原实心图形时配合 stroke-width 使用
- 阈值不理想可调 `threshold`（0-255，默认 128）

## 图片属性探测

```
mcp call vision-mcp_image_stats {"image": "/tmp/xxx.png"}
```

- 返回格式/尺寸/主色 top5——决定下一步：超宽长图 → ocr_image；小图标 → analyze_image + region

## 坑

- 参数名是 `image`，传 `path` 或不传会报 `missing required argument: image`
- 路径必须是服务器可读的本地路径，不是 URL
- 一次一张图，多张分开调用
- `cannot read <path>` 表示路径不存在或权限不足
- 413 报错 → 图片太大，先裁剪/缩放再送
