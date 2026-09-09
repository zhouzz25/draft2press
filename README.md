# 公众号写作排版 Agent

基于 Rust + Web 前端的公众号文稿写作与排版 AI Agent。支持素材上传、AI 初稿生成、批注式局部修订、多模板排版、照片 AI 识图、微信公众号草稿箱推送。

## 界面预览

**写作与批注修订**——左侧管理素材与写作指令，中间预览/编辑文章，右侧对选中文字添加批注做局部修订：

![写作与批注界面](docs/screenshot-writing.png)

**排版预览**——选择模板、上传配图并 AI 识图，右侧手机框实时预览排版效果，可调节字号/行距等细节：

![排版预览界面](docs/screenshot-format.png)

## 快速入门

```bash
# 1. 编译运行
cargo build
cargo run -- --port 3000
```

浏览器打开 `http://localhost:3000`，首次使用点击右上角「设置」按钮填入 API Key 即可，所有配置均可在网页端实时修改，无需重启或编辑配置文件。

（程序首次运行会自动创建 `.env` 文件，如需通过文件配置也可编辑 `.env` 和 `config.toml`，但推荐直接在网页设置页操作。）

## 功能一览

### 写作流程
1. 拖拽上传素材（txt/md/docx/pdf），自动提取文本（PDF 超 10MB 明确拒绝）
2. 输入选题，可选填文章类型
3. 点击「生成初稿」，SSE 流式实时显示「已生成 N 字」进度，可随时中断；LLM 流式失败自动回退非流式
4. 选中文章文字 → 点击「+ 添加」创建批注
5. 填写修改意见 → 「逐条处理批注」：每条独立请求，弹左右双栏 diff（红删改绿新增），逐条接受/放弃/停止；同类问题全文同步修正
6. 可多轮批注修订直到满意
7. 「编辑」模式可直接修改 Markdown 源文
8. 导出 .md 或复制全文

### 排版流程
1. 点击「进入排版」
2. 上传照片（可选），点击「AI 识图」自动生成描述；勾选「必选」的图 AI 必用，「全选」一键全选
3. 选择排版模板——内置模板（每个模板一个文件夹：`template.md` 定义 + `assets/` 本地贴纸素材）或 **AI 生成新模板**：扔一段自然语言 + 推文链接，自动抓取页面贴纸存本地并写好使用方法
4. 点击「生成排版」（模板样式全文注入 prompt，贴纸以 `{{asset:...}}` 浮层贴在标题/卡片旁）
5. 滑块实时调节字号/行距/字距/左右间距/段距
6. 复制 HTML（贴纸转 base64 内嵌，可直接粘贴到秀米/微信编辑器）
7. 下载 ZIP（HTML + images 文件夹，含模板贴纸）
8. 推送草稿箱（贴纸自动走微信素材上传成微信 CDN 链接；标题 AI 自动生成）

### 其他功能
- 保存/加载会话（含文章、批注、素材、照片的完整上下文）
- Debug Console 查看 API 调用日志（system/user/assistant 消息原文 + token 统计）
- Token 统计 + 费用换算（点击切换 USD/CNY；价格单位可选 USD/CNY × /1K/1M）
- Token 预算上限（达到自动中断）
- API Key 密码式输入，保存后不回显（服务端持久化到 config.toml）

## 配置说明

所有配置均可在 Web 设置页修改并自动持久化到 config.toml，也支持文件配置（可选）：

```toml
endpoint = "https://api.deepseek.com/chat/completions"
model = "deepseek-v4-flash"
context_length = 64000
max_tokens = 100000
temperature = 0.7

# Vision 模型（AI 识图，可选）
vision_model = "deepseek-v4-flash-vision-exp"
vision_max_tokens = 4000
vision_desc_max_chars = 30

# 微信公众号（推送草稿箱，可选）
wx_app_id = "your-app-id"
wx_app_secret = "your-app-secret"

# Token 预算（可选，达到上限自动中断）
token_budget = 100000

[pricing]
input_per_1k = 0.00027
output_per_1k = 0.0011
```

`.env` 文件：
```
API_KEY=sk-your-api-key
```

## 技术选型

| crate | 用途 |
|-------|------|
| axum | Web 服务器（SSE 流式响应、multipart 文件上传） |
| reqwest | HTTP 客户端（LLM/vision/微信 API） |
| tokio + tokio-stream | 异步运行时 + SSE 流 |
| serde/serde_json | 序列化（API 响应、会话存取） |
| zip | docx 解析 + ZIP 打包下载 |
| pdf-extract | PDF 文本提取 |
| base64 | 图片编解码 |
| clap | CLI 参数（--port） |
| anyhow | 错误处理 |

## 项目结构

```
src/
  main.rs          # 入口（Web 服务）
  config.rs        # 模型/价格/vision/公众号/token预算 配置
  llm.rs           # OpenAI 兼容 LLM 客户端
  cost.rs          # Token 统计 + 成本换算
  materials.rs     # 素材解析（docx/pdf/txt/md）+ 分块
  writer.rs        # 写作 + 批注修订 prompt
  formatter.rs     # 排版 HTML 生成 + 清洗 + 模板选择
  wechat.rs        # 微信公众号 API（token/上传/草稿）
  server.rs        # axum 路由（SSE/会话/照片/排版/推送/ZIP）
frontend/
  index.html       # 页面结构
  style.css        # 样式（手机边框预览 + CSS 变量调节）
  app.js           # 逻辑（SSE 解析/批注高亮/模板/滑块）
prompts/           # 外置 prompt（改 prompt 不用编译）
  system.md        # 写作系统 prompt
  user_prompt.md   # 用户消息模板
  revision_*.md    # 批注修订 prompt
  template_gen.md  # AI 生成模板的 prompt
  format_system.md # 排版系统 prompt
  templates/       # 排版模板库（一模板一文件夹：template.md + assets/ 贴纸素材）
```



## 演示用例

```bash
# 启动服务
cargo run -- --port 3000

# 1. 浏览器打开 http://localhost:3000
# 2. 设置页填入 API Key（首次使用）
# 3. 上传素材：拖拽 txt/docx/pdf 到左侧上传区
# 4. 输入选题
# 5. 点击「生成初稿」→ 实时看到进度 → 初稿出现
# 6. 选中一段文字 → 添加批注 → 提交 → 修订稿出现
# 7. 点击「进入排版」→ 上传照片 → AI 识图 → 生成排版
# 8. 调节字号/间距滑块 → 复制 HTML 或推送草稿箱
```
