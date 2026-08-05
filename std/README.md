# Bolide 标准库

## 导入

推荐短路径（编译器自动解析到 `std/<name>/<name>.bl`）：

```bolide
import "std/fs" as fs;
import "std/json" as json;
import "std/time" as time;
```

旧路径 `import "std/fs/fs.bl"` 仍然可用。

常用模块可一次导入：

```bolide
import "std/prelude" as std;
// std.fs / std.time / std.json / ...
```

## 模块索引

### 核心 / 语言配套

| 短路径 | 说明 |
|--------|------|
| `std/option` | `Option` 组合子 |
| `std/result` | `Result` 组合子 |
| `std/traits` | 标准协议 trait（Iterator / Display / Add…） |
| `std/macros` | 常用宏（`max2` / `dbg` / `todo`…） |
| `std/assert` | 轻量测试断言 |
| `std/prelude` | 常用模块集合 |

### 数据与算法

| 短路径 | 说明 |
|--------|------|
| `std/collections` | Set / Queue / Stack / Counter / Deque / 优先队列 |
| `std/iter` | 列表工具（take/drop/zip/unique…） |
| `std/sort` | 排序 |
| `std/math` | 数学 |
| `std/random` | 随机数 |
| `std/hash` | FNV / CRC32 |
| `std/vec3` | 3D 向量 |

### 文本与编解码

| 短路径 | 说明 |
|--------|------|
| `std/text` | 字符串处理 |
| `std/buffer` | 可增长文本缓冲 |
| `std/bytes` | 字节缓冲 |
| `std/encoding` | hex / base64 |
| `std/json` | JSON 解析与序列化 |
| `std/csv` | CSV |
| `std/regex` | 正则 |
| `std/template` | HTML 模板 |
| `std/html` | HTML 抽取 |
| `std/table` | 终端表格 |

### 系统

| 短路径 | 说明 |
|--------|------|
| `std/fs` | 文件系统 |
| `std/path` | 路径 |
| `std/io` | 流式写文件 |
| `std/env` | 环境变量 / 参数 / OS |
| `std/process` | 子进程 |
| `std/time` | 时间 / 计时 / 睡眠 |
| `std/log` | 日志 |
| `std/cli` | 命令行参数 |

### 并发

| 短路径 | 说明 |
|--------|------|
| `std/atomic` | 原子类型 |
| `std/sync` | Mutex / RwLock / Once |
| `std/arena` | 值 Arena |

### 网络与应用

| 短路径 | 说明 |
|--------|------|
| `std/http` | HTTP 客户端（`get` / `post_json` / timeout） |
| `std/web` | HTTP 服务 / 路由 / SSE / WebSocket / fetch |
| `std/url` | URL |
| `std/crawler` | 爬虫辅助 |
| `std/cache` | 内存缓存 |
| `std/config` | 配置文件 |
| `std/db` | 内置文件库 |
| `std/sqlite` | SQLite |
| `std/gui` | 桌面 GUI（egui：`run` / `run_default`） |
| `std/cli` | 命令行参数（`parse_or_exit` / `help_flag`） |
| `std/uuid` | UUID / 短 ID |

## 约定

1. **短路径优先**；文档与新代码统一 `import "std/<name>"`。
2. **失败语义**：系统/IO 类 API 多数返回 `bool` 表示成败；需要消息时用 `std/result` 自行包装，或看具体模块文档。
3. **进程结果**：`std/process` 的返回类型为 `ProcessResult`（避免与 ADT `Result` 混淆）；旧名 `Result` 仍作子类兼容。
4. **协议**：`T: Iterator` / `T: Add` 等可依赖方法自动满足；正式定义见 `std/traits`。

更完整的教程见 `docs/standard-library.md`。
