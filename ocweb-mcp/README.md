# OCWeb MCP

独立 Rust 浏览器自动化服务。连接随发布版提供的 Chrome 扩展，通过同一个本地端口提供 **HTTP JSON API、Streamable HTTP MCP、内置 API 文档**。运行不需要 OpenClaw、Gateway、Node.js 或 Playwright。

## 启动

下载适合当前系统的可执行文件和 Chrome 扩展包，无需源码或 Rust 环境。在可执行文件所在目录启动服务并保持运行：

```sh
# macOS / Linux
./ocweb-mcp
```

```powershell
# Windows PowerShell
.\ocweb-mcp.exe
```

打开终端显示的文档地址（默认 <http://127.0.0.1:18799/docs>），即可查看实时连接状态并复制完整连接串。

默认监听 `127.0.0.1:18799`。先监听再等待扩展；5 秒未连接时打印操作指引，进程继续运行。扩展连接成功后打印 MCP 接入方式；断线后继续等待重连。

```sh
./ocweb-mcp --port 18800 --connect-timeout 10
```

`--port 0` 自动选择空闲端口，主要用于测试。重新选择端口后需要重新配对。`--state-dir <directory>` 更改凭据目录，默认为 `~/.ocweb-mcp`。使用 `--help` 查看参数。

凭据在首次启动时生成，保存在 `credentials.json`，Unix 上权限为 0600。`relay_key` 只用于扩展配对，`api_token` 用于脚本及 MCP。正常日志不会输出密钥；本机 `/docs` 通过 `/setup` 动态获取完整配对连接串，请勿分享连接串。该接口不返回 API token，响应禁止缓存。`--show-credentials` 会在本地终端显式显示这两个秘密，请勿分享其输出。重启保留凭据，不需要每次重新配对。

也可在服务已运行时，另开终端使用可执行文件获取配对串，不会启动第二个服务（Windows PowerShell 将 `./ocweb-mcp` 换成 `.\ocweb-mcp.exe`）：

```sh
./ocweb-mcp --print-pairing
# macOS：直接复制到剪贴板
./ocweb-mcp --print-pairing | pbcopy
```

如果使用自定义端口或凭据目录，上述命令也要传相同的 `--port` / `--state-dir`。更新已加载的扩展后，在 `chrome://extensions` 点击其重新加载按钮。

## 首次连接 Chrome

1. 解压下载的 Chrome 扩展包，打开 `chrome://extensions`，开启开发者模式。
2. 点击“加载已解压的扩展程序”，选择解压后包含 `manifest.json` 的目录。
3. 打开服务终端显示的 `/docs` 页面，点击“复制连接串”。打开扩展的 Settings，在 **Connect to OCWeb** 中粘贴该完整连接串并点击 **Connect**。
4. 选择 **Selected tabs** 并把页面加入 **OpenClaw** 标签组，或选择 **All tabs**。页面必须是扩展可访问的普通标签页。
5. 文档页面自动显示“浏览器已连接”，终端也会提示。空标签列表也可以代表连接成功；请检查授权范围。

Chrome 扩展主动连接 Rust WebSocket 服务。服务不扫描个人浏览器配置、不启用远程调试端口、不自动安装或重启浏览器。保留扩展的配对认证及标签页授权规则。

## 在 AI 中添加 MCP

本地 URL：`http://127.0.0.1:18799/mcp`，传输为 **Streamable HTTP**，设置请求头 `Authorization: Bearer <api_token>`。支持自定义 HTTP MCP 的客户端可按其配置格式添加，例如：

```json
{
  "mcpServers": {
    "ocweb": {
      "url": "http://127.0.0.1:18799/mcp",
      "headers": { "Authorization": "Bearer <api_token>" }
    }
  }
}
```

这是通用配置示意，客户端的外层字段可能不同。不要把配对用的 `relay_key` 当作 API token。仅接受 OAuth 的云端连接器不能直接接入；本机 loopback 也不能直接被云端访问。当前版本不提供 stdio。

MCP 使用官方 Rust SDK [rmcp](https://github.com/modelcontextprotocol/rust-sdk) 处理初始化、协议协商、工具发现、Streamable HTTP 与会话。`browser` 工具的参数与 HTTP API 的操作 JSON 完全相同，JSON Schema 自动生成。截图通过 MCP image content 返回。

## HTTP API（无需 MCP）

- 文档：<http://127.0.0.1:18799/docs>，内置完整说明、示例和只读试用，不依赖外部 CDN。
- OpenAPI 3.1：<http://127.0.0.1:18799/openapi.json>。
- 状态：`GET /health`，公开的最小服务状态，不包含标签页内容。
- 标签页：`GET /api/tabs`，需要 Bearer token。
- 浏览器操作：`POST /api/browser`，需要 Bearer token。

```sh
export OCWEB_TOKEN='<api_token>'
curl -H "Authorization: Bearer $OCWEB_TOKEN" http://127.0.0.1:18799/api/tabs

curl http://127.0.0.1:18799/api/browser \
  -H "Authorization: Bearer $OCWEB_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"action":"open","url":"https://example.com"}'
```

使用返回的 `tabId` 作为下列操作的 `tab_id`：

```json
{"action":"snapshot","tab_id":123}
{"action":"type","tab_id":123,"target":{"selector":"#search"},"text":"Rust"}
{"action":"click","tab_id":123,"target":{"node_ref":"从 snapshot 复制"}}
{"action":"press","tab_id":123,"key":"Enter"}
{"action":"wait","tab_id":123,"expression":"document.readyState === 'complete'"}
{"action":"evaluate","tab_id":123,"expression":"document.title"}
{"action":"screenshot","tab_id":123,"full_page":false}
{"action":"close","tab_id":123}
```

还支持 `tabs`、`navigate`、`scroll`、`focus`、`cdp`，全部参数见内置文档与 OpenAPI。截图 REST 结果是 `{ "mime_type": "image/png", "data": "<base64>" }`。

## 实现边界

- `auth.rs`：兼容扩展 Browser Relay Authentication v2 的双向 HMAC 挑战应答与持久凭据。
- `relay.rs`：WebSocket 生命周期、命令/响应关联、心跳、超时和断线处理。
- `browser.rs`：共用浏览器操作层，直接调用 Chrome CDP，不需要 Playwright。
- `mcp.rs` / `server.rs`：MCP 与 HTTP 适配，共用操作实现。
- `static/docs.html`：编译进二进制的 API 文档。

浏览器端使用现有的扩展，Rust 直接实现扩展消息协议，没有移植 OpenClaw 的整个 CDP/Playwright 桥。因此这是 **浏览器操作 API + MCP 服务**，不是可供 `playwright.connectOverCDP()` 直接连接的完整 CDP WebSocket 代理。

节点引用绑定连接、标签页和文档；导航或重连后旧引用报错。CSS selector 面向主文档，复杂 iframe / Shadow DOM 可通过 `evaluate` / `cdp` 处理。截图会激活目标标签页以取得绘制帧，且大小有限制。没有额外实现下载管理、文件上传或 Playwright locator 引擎。任意 JavaScript/CDP 能力仅对持有 API token 的客户端开放。

所有接口只监听 loopback；校验 Host 和 Origin，不开放跨域 CORS。API/MCP 要求独立 token，扩展必须完成 v2 握手。REST 和 MCP 共享操作队列，防止多步输入交错；仍与用户及其他调试客户端共享页面。单条中继命令超时为 30 秒，断线或超时不会自动重放写操作，错误提示要求先检查页面结果。

## 页面变化与安全重试

`evaluate` 支持 `safe_retry`，默认 `false`，REST 和 MCP 使用相同参数：

```json
{"action":"evaluate","tab_id":123,"expression":"document.title","safe_retry":true}
```

`safe_retry: true` 表示调用方确认表达式可安全重复执行，例如读取页面状态。后端只对 `DOCUMENT_CHANGED` 重试，最多执行 5 次，总计最多 30 秒（从取得操作队列开始计算）。每次重新检查当前权限，始终绑定原浏览器连接和标签页。持续跳转可能耗尽次数，返回 `DOCUMENT_CHANGED`；超过时间返回 `SAFE_RETRY_TIMEOUT`。不保证页面一定恢复。

`safe_retry: false` 时不重放表达式；页面在执行期间变化会返回 `DOCUMENT_CHANGED`，而不是 `revoked`。该错误可能发生在表达式已执行之后，因此开启重试前必须确认重复执行安全。点击、输入、提交、修改页面等操作默认不重试，也不接受这个参数。

真正撤销权限返回 `ACCESS_REVOKED`；断线、普通 JavaScript 异常和其他错误不自动重试。REST 保留 `error` 文本并附加 `code`，MCP 的错误 text 内容为同样的 JSON 对象（`isError: true`）。升级时需要重启 Rust 服务并在 `chrome://extensions` 重新加载扩展，旧扩展没有结构化错误码，服务不会根据旧错误文本猜测是否重试。无法分类的旧式访问错误返回一般 `BROWSER_ERROR` 和重新加载扩展的指引，不再声称权限已被撤销。

## 源码开发与验证

以下仅供源码开发者使用，发布版用户无需安装 Rust 或执行 Cargo 命令。

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build
# 构建发布版：cargo build --release
CHROME_BIN='/path/to/Chrome for Testing' node scripts/smoke.mjs
# 只验证导航错误分类、REST/MCP 安全重试、重试上限及真实撤权：
CHROME_BIN='/path/to/Chrome for Testing' node scripts/smoke.mjs --navigation-only
```

`smoke.mjs` 只在验证时需要 Node 22+。它创建临时 Chrome 配置和凭据，加载真正的扩展，通过真实 HTTP/MCP 请求验证认证、页面读取、原生点击/输入、截图、导航后旧引用失效及断线处理；测试退出时清理自己的进程和目录。它不读取或修改日常 Chrome 配置。本机导航专项实测耗时约 4.39 秒（Chrome for Testing 147，包含浏览器启动和清理）。测试中的有界就绪等待用于外部浏览器启动，不模拟已完成的握手或页面操作。

## 来源

浏览器扩展及认证协议来源于 OpenClaw（MIT）。本目录保留其版权和许可声明，见 `LICENSE`。依赖各自遵循其许可证。
