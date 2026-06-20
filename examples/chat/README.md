# Bolide 在线聊天室

这是一个用 Bolide Web 标准库写的在线聊天室示例。

前端页面不是通过 `static_files` 自动托管，而是由 Bolide 程序的 `/` 路由读取 `public/index.html` 后返回：

```bolide
fn index(req: web.Request) -> web.Response {
    let res: web.Response = web.html(fs.read_text(path));
    res.set_header("Cache-Control", "no-store");
    return res;
}
```

这样前端 HTML/CSS/JS 保持为独立文件，便于维护；响应仍然由 Bolide handler 显式发出。

## 开发运行

在仓库根目录运行：

```powershell
cargo run -p bolide-cli -- run examples\chat\main.bl
```

打开：

```text
http://127.0.0.1:8020
```

可以打开两个浏览器窗口测试多人聊天。

## Release 编译

```powershell
cargo build --release
target\release\bolide.exe compile examples\chat\main.bl --output tmp\bolide_chat_file_server_v2.exe
```

## 发布目录

如果不在仓库根目录运行 exe，请使用下面的目录结构：

```text
chat-dist/
  bolide_chat.exe
  public/
    index.html
```

程序会优先读取：

```text
examples/chat/public/index.html
```

如果不存在，会读取：

```text
public/index.html
```

因此发布时把 `examples/chat/public/index.html` 复制到 exe 同级的 `public/index.html` 即可。

## 路由

- `GET /`：读取并返回聊天室 HTML 页面。
- `GET /api/health`：返回服务健康状态。
- `GET /api/messages?after=N`：返回 ID 大于 `N` 的消息。
- `POST /api/send`：发送消息，表单字段为 `name` 和 `text`。

## 实现说明

- 消息保存在内存列表里，最多保留最近 200 条。
- 服务端使用 `app.set_workers(1)`，确保内存消息列表由单个 reactor worker 修改。
- 前端使用 HTTP 增量轮询，每秒同步一次消息。
- 等 runtime 提供线程安全的应用级 WebSocket 广播注册表后，可以把 `/api/messages` 轮询替换成真正的 WebSocket 广播。
