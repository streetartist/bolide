# Bolide 标准库教程

本文档覆盖当前仓库 `std/` 目录下的标准库，面向想用 Bolide 写脚本、小工具、Web 服务、GUI 程序和数据处理程序的开发者。

标准库都用普通 Bolide 模块形式发布，导入方式统一：

```bolide
import "std/fs/fs.bl" as fs;
import "std/json/json.bl" as json;
```

运行示例：

```bash
bolide run examples/demo.bl
bolide compile examples/demo.bl -o demo
```

## 模块总览

| 模块 | 路径 | 用途 |
| --- | --- | --- |
| `assert` | `std/assert/assert.bl` | 测试断言和简单测试输出 |
| `cache` | `std/cache/cache.bl` | 内存缓存，支持 TTL |
| `cli` | `std/cli/cli.bl` | 命令行参数解析和帮助文本 |
| `config` | `std/config/config.bl` | 简单 key-value 配置读取 |
| `crawler` | `std/crawler/crawler.bl` | 网页抓取、链接提取、队列去重 |
| `csv` | `std/csv/csv.bl` | CSV 生成和解析 |
| `db` | `std/db/db.bl` | Bolide 内置文件数据库 |
| `encoding` | `std/encoding/encoding.bl` | bytes 的 hex/base64 编解码 |
| `env` | `std/env/env.bl` | 环境变量、命令行参数、系统信息 |
| `fs` | `std/fs/fs.bl` | 文件和目录操作 |
| `gui` | `std/gui/gui.bl` | 桌面 GUI |
| `html` | `std/html/html.bl` | HTML 文本、链接、图片、meta 提取 |
| `http` | `std/http/http.bl` | HTTP 客户端便捷封装 |
| `json` | `std/json/json.bl` | 安全生成 JSON 字符串 |
| `log` | `std/log/log.bl` | 日志输出和文件日志 |
| `math` | `std/math/math.bl` | 数学函数 |
| `path` | `std/path/path.bl` | 路径处理 |
| `process` | `std/process/process.bl` | 执行外部进程 |
| `random` | `std/random/random.bl` | 随机数和随机选择 |
| `regex` | `std/regex/regex.bl` | 正则匹配、提取、替换、切分 |
| `sqlite` | `std/sqlite/sqlite.bl` | SQLite 数据库 |
| `table` | `std/table/table.bl` | 命令行表格格式化 |
| `template` | `std/template/template.bl` | HTML 模板渲染 |
| `text` | `std/text/text.bl` | 常用文本处理 |
| `time` | `std/time/time.bl` | 时间戳、单调时间、睡眠 |
| `url` | `std/url/url.bl` | URL 解析、编码、查询串、相对链接解析 |
| `uuid` | `std/uuid/uuid.bl` | UUID v4 和短 ID |
| `web` | `std/web/web.bl` | HTTP 服务、路由、会话、流式响应、WebSocket |

## 选择哪个库

写命令行工具：

```bolide
import "std/cli/cli.bl" as cli;
import "std/fs/fs.bl" as fs;
import "std/path/path.bl" as path;
import "std/log/log.bl" as log;
```

写爬虫或 API 调用：

```bolide
import "std/http/http.bl" as http;
import "std/crawler/crawler.bl" as crawler;
import "std/html/html.bl" as html;
import "std/url/url.bl" as url;
import "std/cache/cache.bl" as cache;
```

写 Web 应用：

```bolide
import "std/web/web.bl" as web;
import "std/json/json.bl" as json;
import "std/template/template.bl" as template;
import "std/sqlite/sqlite.bl" as sqlite;
```

写数据处理脚本：

```bolide
import "std/csv/csv.bl" as csv;
import "std/regex/regex.bl" as regex;
import "std/text/text.bl" as text;
import "std/table/table.bl" as table;
```

## assert: 测试断言

`std/assert` 适合给标准库、小脚本和示例写轻量测试。它会打印 `ok` 或 `FAIL`，并维护通过和失败计数。

```bolide
import "std/assert/assert.bl" as assert;

assert.reset();
assert.equal("sum", 1 + 2, 3);
assert.is_true("contains", "bolide".contains("lid"));
assert.contains("message", "hello bolide", "bolide");

print(assert.summary() + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `reset()` | 清空计数 |
| `pass(name)` | 手动记录通过 |
| `fail(name)` | 手动记录失败 |
| `fail_with(name, detail)` | 带细节的失败 |
| `equal(name, got, want)` | 支持 `str/int/float/bool` 重载 |
| `is_true(name, got)` | 断言 bool 为真 |
| `is_false(name, got)` | 断言 bool 为假 |
| `contains(name, text, needle)` | 断言字符串包含 |
| `not_contains(name, text, needle)` | 断言字符串不包含 |
| `passed_count()` | 通过数量 |
| `failed_count()` | 失败数量 |
| `ok()` | 是否没有失败 |
| `summary()` | 返回摘要字符串 |

## cache: 内存缓存

`std/cache` 提供基于 `dict<str, dynamic>` 的内存缓存，支持永久值和 TTL 值。适合 API 缓存、爬虫去重、小服务状态缓存。

```bolide
import "std/cache/cache.bl" as cache;
import "std/time/time.bl" as time;

let c: cache.Cache = cache.new();
c.set("name", "bolide");
c.set_ttl("token", "abc", 5000);

if c.contains("token") {
    print(str(c.get_or("token", "")) + "\n");
}

time.sleep_ms(6000);
print(str(c.contains("token")) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `cache.new()` | 创建空缓存 |
| `cache.with_value(key, value)` | 创建并放入一个值 |
| `Cache.set(key, value)` | 设置永不过期值 |
| `Cache.set_ttl(key, value, ttl_ms)` | 设置带 TTL 的值 |
| `Cache.get_or(key, fallback)` | 读取值，缺失时返回 fallback |
| `Cache.contains(key)` | 检查存在且未过期 |
| `Cache.remove(key)` | 删除 key |
| `Cache.clear()` | 清空 |
| `Cache.cleanup()` | 清理过期项，返回删除数量 |
| `Cache.len()` | 当前有效项数量 |
| `Cache.keys()` | 当前有效 key |
| `Cache.touch(key, ttl_ms)` | 延长或修改 TTL |
| `Cache.ttl_ms(key)` | 剩余毫秒，`-1` 表示不存在或永久 |

## cli: 命令行参数

`std/cli` 支持长选项、短选项、必填选项、默认值、位置参数和帮助文本。

```bolide
import "std/cli/cli.bl" as cli;
import "std/env/env.bl" as env;

let specs: list<cli.Spec> = [
    cli.option("file", "f", "PATH", "input.txt", "input file"),
    cli.flag("verbose", "v", "verbose output"),
    cli.required_option("name", "n", "NAME", "project name")
];

let args: cli.Args = cli.parse(env.args(), specs);
if args.has_errors() {
    print(args.error_text() + "\n");
    print(cli.help(args.program, "demo tool", specs));
    env.exit(1);
}

print(args.value("file") + "\n");
print(str(args.flag("verbose")) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `flag(name, short, help)` | 创建布尔选项 |
| `option(name, short, value_name, default_value, help)` | 创建带值选项 |
| `required_option(name, short, value_name, help)` | 创建必填选项 |
| `parse(argv, specs)` | 解析实际命令行 |
| `parse_values(values, specs)` | 解析给定列表 |
| `parse_env(specs)` | 从 `env.args()` 解析 |
| `parse_simple(argv)` | 不使用 spec 的简易解析 |
| `help(program, description, specs)` | 生成帮助文本 |

`Args` 常用方法：

| 方法 | 说明 |
| --- | --- |
| `has(name)` | 是否提供过选项 |
| `flag(name)` | 读取布尔选项 |
| `value(name)` | 读取字符串值，缺失为空字符串 |
| `value_or(name, fallback)` | 读取字符串值 |
| `int_or(name, fallback)` | 读取整数值 |
| `positional(index)` | 读取位置参数 |
| `positional_count()` | 位置参数数量 |
| `has_errors()` | 是否有解析错误 |
| `error_text()` | 错误文本 |

## config: 简单配置

`std/config` 读取简单 key-value 配置。适合 `.env`、小工具配置和默认值管理。

```bolide
import "std/config/config.bl" as config;

let cfg: config.Config = config.parse("host=127.0.0.1\nport=8080\ndebug=true\n");

print(config.get_or(cfg, "host", "localhost") + "\n");
print(str(config.get_int(cfg, "port", 80)) + "\n");
print(str(config.get_bool(cfg, "debug", false)) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `parse(text)` | 从字符串解析 |
| `load(path)` | 从文件读取 |
| `load_env(path)` | 读取并写入环境变量 |
| `get(cfg, key)` | 读取字符串，缺失为空 |
| `get_or(cfg, key, fallback)` | 读取字符串 |
| `get_int(cfg, key, fallback)` | 读取整数 |
| `get_bool(cfg, key, fallback)` | 读取布尔 |

## crawler: 爬虫辅助

`std/crawler` 在 `web`、`html`、`url` 之上提供抓取、队列和链接解析。

```bolide
import "std/crawler/crawler.bl" as crawler;

let q: crawler.Queue = crawler.queue();
q.add("https://example.com/");

let opts: crawler.Options = crawler.default_options();
while q.has_next() and q.seen_count() < 10 {
    let page: crawler.Page = crawler.crawl_once(q, opts, true);
    if crawler.ok(page) {
        print(str(page.status) + " " + page.url + " " + crawler.page_title(page) + "\n");
    }
}
```

常用 API：

| API | 说明 |
| --- | --- |
| `default_options()` | 默认抓取选项 |
| `options(timeout_ms, max_redirects, delay_ms, user_agent)` | 自定义选项 |
| `queue()` | 创建去重队列 |
| `fetch(target, opts)` | 抓取页面 |
| `ok(page)` | 状态码是否 2xx |
| `links(base_url, body)` | 提取并解析链接 |
| `internal_links(base_url, body)` | 只保留同 host 链接 |
| `same_host(a, b)` | 判断同 host |
| `save(page, path)` | 保存页面正文 |
| `page_title(page)` | 提取标题 |
| `crawl_once(q, opts, same_site_only)` | 从队列抓一个页面并加入新链接 |

`Queue` 方法：`add`、`add_all`、`has_next`、`next`、`pending_count`、`seen_count`。

## csv: CSV 读写

`std/csv` 处理常见 RFC4180 风格 CSV：逗号、引号、双引号转义和多行字段。

```bolide
import "std/csv/csv.bl" as csv;

let data: str = csv.stringify([
    ["name", "note"],
    ["bolide", "hello, csv"],
    ["quote", "a\"b"]
]);

print(data + "\n");

let rows: list<list<str>> = csv.parse(data);
print(rows[1][0] + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `escape(value)` | 转义单个字段 |
| `line(values)` | 生成一行 |
| `parse_line(line)` | 解析一行 |
| `parse(text)` | 解析完整 CSV |
| `stringify(rows)` | 生成完整 CSV |

## db: 内置文件数据库

`std/db` 是 Bolide 的简单嵌入式文件数据库，适合示例、小应用和无需 SQL 的结构化数据。

```bolide
import "std/db/db.bl" as db;

let store: db.Database = db.open("data/app.bdb");
store.create_table("posts", "title,body,published");

let id: int = store.insert("posts", {
    "title": "Hello",
    "body": "Bolide DB",
    "published": true
});

let row: dict<str, dynamic> = store.get("posts", id);
print(str(row["title"]) + "\n");

store.close();
```

常用 API：

| API | 说明 |
| --- | --- |
| `open(path)` | 打开数据库 |
| `Database.create_table(table, columns)` | 创建表 |
| `Database.insert(table, row)` | 插入并返回 id |
| `Database.update(table, id, row)` | 更新 |
| `Database.delete(table, id)` | 删除 |
| `Database.get(table, id)` | 根据 id 获取 |
| `Database.all(table)` | 获取所有行 |
| `Database.where_eq(table, column, value)` | 等值查询 |
| `Database.count(table)` | 行数 |
| `Database.last_error()` | 最近错误 |
| `Database.close()` | 关闭 |

模块级函数 `create_table`、`insert`、`update`、`delete`、`get`、`all`、`where_eq`、`count` 是同名方法的包装。

## encoding: 字节编码

`std/encoding` 面向 `bytes`，提供 hex 和 base64。它不负责字符串 UTF-8 编码转换。

```bolide
import "std/encoding/encoding.bl" as encoding;

let data: bytes = bytes();
data.push(65);
data.push(66);
data.push(67);

print(encoding.hex_encode(data) + "\n");      // 414243
print(encoding.base64_encode(data) + "\n");   // QUJD

let decoded: bytes = encoding.base64_decode("QUJD");
print(str(decoded[0]) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `hex_encode(data)` | bytes 转小写 hex |
| `hex_decode(text)` | hex 转 bytes，忽略非 hex 字符 |
| `base64_encode(data)` | bytes 转 base64 |
| `base64_decode(text)` | base64 转 bytes，忽略空白 |

## env: 环境和系统信息

```bolide
import "std/env/env.bl" as env;

print(env.os() + " " + env.arch() + "\n");
print(env.current_exe() + "\n");

env.set("BOLIDE_DEMO", "1");
if env.contains("BOLIDE_DEMO") {
    print(env.get("BOLIDE_DEMO") + "\n");
}
env.remove("BOLIDE_DEMO");
```

常用 API：

| API | 说明 |
| --- | --- |
| `get(key)` | 读取环境变量，缺失为空 |
| `get_or(key, fallback)` | 读取环境变量 |
| `contains(key)` | 是否存在 |
| `set(key, value)` | 设置 |
| `remove(key)` | 删除 |
| `args()` | 命令行参数 |
| `vars()` | 所有环境变量，格式 `KEY=VALUE` |
| `current_exe()` | 当前可执行文件 |
| `temp_dir()` | 临时目录 |
| `home_dir()` | 用户目录 |
| `os()` | 操作系统 |
| `arch()` | 架构 |
| `family()` | 系统族 |
| `exe_suffix()` | 可执行文件后缀 |
| `exit(code)` | 退出进程 |

## fs: 文件系统

```bolide
import "std/fs/fs.bl" as fs;

fs.create_dir_all("tmp/demo");
fs.write_text("tmp/demo/hello.txt", "hello\nbolide\n");

let lines: list<str> = fs.read_lines("tmp/demo/hello.txt");
print(lines[0] + "\n");

for name in fs.read_dir("tmp/demo") {
    print(name + "\n");
}
```

常用 API：

| API | 说明 |
| --- | --- |
| `read_text(path)` | 读取文本 |
| `read_bytes(path)` | 读取 bytes |
| `read_lines(path)` | 读取文本行 |
| `write_text(path, content)` | 写文本 |
| `write_bytes(path, data)` | 写 bytes |
| `append_text(path, content)` | 追加文本 |
| `append_bytes(path, data)` | 追加 bytes |
| `touch(path)` | 创建或更新时间 |
| `exists(path)` | 是否存在 |
| `is_file(path)` | 是否文件 |
| `is_dir(path)` | 是否目录 |
| `is_symlink(path)` | 是否符号链接 |
| `remove_file(path)` | 删除文件 |
| `copy(src, dst)` | 复制，返回字节数或状态 |
| `rename(src, dst)` | 重命名 |
| `create_dir(path)` | 创建目录 |
| `create_dir_all(path)` | 递归创建目录 |
| `remove_dir(path)` | 删除空目录 |
| `remove_dir_all(path)` | 递归删除目录 |
| `read_dir(path)` | 读取目录项 |
| `file_name(path)` | 文件名 |
| `parent(path)` | 父目录 |
| `extension(path)` | 扩展名 |
| `stem(path)` | 文件主名 |
| `join(base, child)` | 拼接路径 |
| `canonicalize(path)` | 规范绝对路径 |
| `current_dir()` | 当前目录 |
| `set_current_dir(path)` | 设置当前目录 |
| `len(path)` | 文件长度 |
| `modified(path)` | 修改时间 |
| `created(path)` | 创建时间 |
| `readonly(path)` | 是否只读 |
| `set_readonly(path, readonly)` | 设置只读 |

## gui: 桌面界面

`std/gui` 基于即时模式 UI。程序提供一个 `view(ui)` 函数，每一帧由 `gui.run` 调用。

```bolide
import "std/gui/gui.bl" as gui;

let count: int = 0;

fn view(ui: gui.Ui) {
    ui.heading("Counter");
    ui.label("count = " + str(count));
    if ui.button("+1") {
        count = count + 1;
    }
}

gui.run("Bolide Counter", 360, 220, view);
```

常用控件：

| 方法 | 说明 |
| --- | --- |
| `label`、`heading`、`small`、`strong` | 文本 |
| `separator`、`space` | 分隔和留白 |
| `button`、`selectable`、`link` | 交互控件 |
| `text_input`、`password_input`、`multiline_input` | 输入 |
| `checkbox`、`slider`、`progress` | 表单控件 |

布局方法：

| 方法 | 说明 |
| --- | --- |
| `row`、`column`、`group`、`grid` | 基础布局 |
| `frame`、`scroll`、`indent`、`collapsing` | 容器 |
| `centered`、`align`、`left`、`right` | 对齐 |
| `pad`、`width`、`height`、`size` | 尺寸约束 |
| `fill_width`、`fill_height`、`fill` | 填充 |
| `place` | 固定区域布局 |
| `pack_top/left/right/bottom` | 边缘布局 |
| `available_width`、`available_height` | 可用空间 |
| `request_repaint` | 请求重绘 |

模块函数：

| API | 说明 |
| --- | --- |
| `backend()` | 当前 GUI 后端 |
| `run(title, width, height, view)` | 启动 GUI |

## html: HTML 提取

`std/html` 是轻量 HTML 辅助工具，不是完整浏览器 DOM。它适合爬虫中提取标题、链接、图片、meta 和简单标签内容。

```bolide
import "std/html/html.bl" as html;

let page: str = "<html><head><title>Hi</title></head><body><a href=\"/x\">x</a></body></html>";

print(html.title(page) + "\n");
for href in html.links(page) {
    print(href + "\n");
}
```

常用 API：

| API | 说明 |
| --- | --- |
| `decode_entities(text)` | 解码常见 HTML 实体 |
| `strip_tags(source)` | 移除标签 |
| `text(source)` | 移除标签并解码实体 |
| `element(source, tag)` | 找第一个元素 |
| `elements(source, tag)` | 找所有元素 |
| `title(source)` | 页面标题 |
| `links(source)` | 所有 `<a href>` |
| `images(source)` | 所有 `<img src>` |
| `meta(source, name)` | meta content |

`Element` 字段：`name`、`open_tag`、`inner`。

## http: HTTP 客户端便捷封装

`std/http` 是 `std/web` 客户端能力的轻量包装。需要完整控制时用 `web.fetch_with_options`。

```bolide
import "std/http/http.bl" as http;
import "std/json/json.bl" as json;

let body: str = json.object([
    json.pair("text", json.value("hello"))
]);

let res: http.Response = http.post_json("https://example.com/api", body);
if res.ok() {
    print(res.text() + "\n");
} elif res.has_error() {
    print("request failed: " + res.error + "\n");
} else {
    print("HTTP " + str(res.status) + "\n");
}
```

常用 API：

| API | 说明 |
| --- | --- |
| `header(name, value)` | 创建 header |
| `header_lines(headers)` | 生成 HTTP header 文本 |
| `json_headers()` | JSON 请求头 |
| `text_headers()` | text 请求头 |
| `request(method, url, body, headers)` | 默认超时和跳转请求 |
| `request_with_options(method, url, body, headers, timeout_ms, max_redirects)` | 完整请求 |
| `get(url)` | GET |
| `get_with_headers(url, headers)` | 带 header 的 GET |
| `post(url, body, content_type)` | POST |
| `post_json(url, body)` | JSON POST |
| `put_json(url, body)` | JSON PUT |
| `patch_json(url, body)` | JSON PATCH |
| `delete(url)` | DELETE |

`Response` 字段和方法：`status`、`body`、`content_type`、`error`、`ok()`、`text()`、`has_error()`。

`status` 是 HTTP 响应状态码。DNS、连接、TLS、超时、非法 URL 等请求层错误会返回 `status == 0`，并把原因放入 `error`。HTTP 404/500 是正常 HTTP 响应，不会写入 `error`。

## json: 安全生成 JSON

`std/json` 当前重点是生成 JSON，不是完整解析器。它能避免手写字符串拼接时的转义错误。

```bolide
import "std/json/json.bl" as json;

let body: str = json.object([
    json.pair("name", json.value("Bolide")),
    json.pair("ok", json.value(true)),
    json.pair("count", json.value(3)),
    json.pair("tags", json.string_array(["web", "json"]))
]);

print(body + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `escape(value)` | JSON 字符串转义，不带引号 |
| `quote(value)` | JSON 字符串，带引号 |
| `null()` | `null` |
| `value(bool/int/float/str)` | 重载生成 JSON 值 |
| `pair(key, value_json)` | 对象字段，值必须已是 JSON |
| `raw_array(items)` | 拼接 JSON 片段数组 |
| `string_array`、`int_array`、`float_array`、`bool_array` | 类型数组 |
| `object(pairs)` | JSON 对象 |
| `dict_str`、`dict_int`、`dict_bool` | 字典转对象 |
| `dict_json(values)` | 字典值已是 JSON 片段 |

## log: 日志

```bolide
import "std/log/log.bl" as log;

log.set_debug();
log.info("server starting");
log.debug("debug detail");

log.set_file("app.log");
log.warn("written to stdout and file");
```

常用 API：

| API | 说明 |
| --- | --- |
| `set_level(level)` | 设置等级 |
| `set_debug()`、`set_info()`、`set_warn()`、`set_error()` | 设置等级 |
| `set_file(path)` | 开启文件日志 |
| `clear_file()` | 关闭文件日志 |
| `debug`、`info`、`warn`、`error` | 输出日志 |
| `write(level, message)` | 按等级输出 |
| `level_name(level)` | 等级名 |

## math: 数学函数

```bolide
import "std/math/math.bl" as math;

print(str(math.max(3, 9)) + "\n");
print(str(math.sqrt(9.0)) + "\n");
print(str(math.clamp(15, 0, 10)) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `PI`、`E` | 常量 |
| `abs`、`min`、`max`、`clamp` | 支持 int/float 重载 |
| `floor`、`ceil`、`round`、`trunc` | 取整 |
| `sqrt`、`pow` | 幂和根 |
| `sin`、`cos`、`tan` | 三角函数 |
| `log`、`ln`、`exp` | 对数和指数 |

## path: 路径字符串处理

`std/path` 处理路径字符串，不直接访问文件系统。需要文件系统信息时用 `std/fs`。

```bolide
import "std/path/path.bl" as path;

let p: str = path.join("src/../tmp", "demo.txt");
print(path.normalize(p) + "\n");
print(path.extension(p) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `normalize_separators(path)` | 统一分隔符 |
| `is_absolute(path)` | 是否绝对路径 |
| `normalize(path)` | 规范化 `.` 和 `..` |
| `join(base, child)` | 拼接两个路径 |
| `join3(a, b, c)` | 拼接三个路径 |
| `file_name`、`parent`、`extension`、`stem` | 路径拆分 |
| `with_extension(path, ext)` | 修改扩展名 |
| `without_extension(path)` | 去掉扩展名 |
| `current_dir()` | 当前目录 |
| `canonicalize(path)` | 规范绝对路径 |

## process: 外部进程

```bolide
import "std/process/process.bl" as process;

let r: process.Result = process.run("bolide", ["--version"]);
print(str(r.status()) + "\n");
print(r.stdout());
print(r.stderr());
```

常用 API：

| API | 说明 |
| --- | --- |
| `run(program, args)` | 执行程序 |
| `run0(program)` | 无参数执行 |
| `shell(command)` | 通过系统 shell 执行 |

`Result` 方法：`status()`、`stdout()`、`stderr()`、`success()`、`close()`。

## random: 随机数

```bolide
import "std/random/random.bl" as random;

random.seed(12345);
print(str(random.rand_int(100)) + "\n");
print(random.choice_str(["red", "green", "blue"]) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `seed(value)` | 固定随机种子 |
| `rand_int(max)` | `[0, max)` |
| `range(min, max)` | `[min, max)` |
| `rand_float()` | `[0.0, 1.0)` |
| `rand_bool()` | 随机 bool |
| `chance(probability)` | 按概率返回 true |
| `choice_int(items)` | 随机 int 项 |
| `choice_str(items)` | 随机 str 项 |

## regex: 正则表达式

`std/regex` 绑定 Rust `regex`，适合验证、提取、替换和切分文本。

```bolide
import "std/regex/regex.bl" as regex;

let text: str = "id=42 name=bolide";

if regex.is_match("\\d+", text) {
    print(regex.find("\\d+", text) + "\n");
}

let caps: list<str> = regex.captures("name=([a-z]+)", text);
print(caps[1] + "\n");

print(regex.replace_all("\\d+", text, "#") + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `is_valid(pattern)` | 正则是否有效 |
| `escape(text)` | 转义为字面量正则 |
| `is_match(pattern, text)` | 是否匹配 |
| `contains(pattern, text)` | `is_match` 别名 |
| `find(pattern, text)` | 第一个匹配，缺失为空 |
| `find_all(pattern, text)` | 所有匹配 |
| `captures(pattern, text)` | 第一组捕获，`[0]` 是完整匹配 |
| `replace(pattern, text, replacement)` | 替换第一个 |
| `replace_all(pattern, text, replacement)` | 替换所有 |
| `split(pattern, text)` | 按正则切分 |

## sqlite: SQLite

`std/sqlite` 适合需要 SQL、查询和持久化的小应用。参数使用 `list<dynamic>`。

```bolide
import "std/sqlite/sqlite.bl" as sqlite;

let db: sqlite.Connection = sqlite.open("tmp/app.db");
db.execute("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)");
db.execute("INSERT INTO users (name, age) VALUES (?, ?)", ["Alice", 30]);

let rows: list<dict<str, dynamic>> = db.query("SELECT name, age FROM users WHERE age > ?", [20]);
for row in rows {
    print(str(row["name"]) + " " + str(row["age"]) + "\n");
}

db.close();
```

`Connection` 常用方法：

| 方法 | 说明 |
| --- | --- |
| `execute(sql)` | 执行 SQL |
| `execute(sql, params)` | 带参数执行 |
| `query(sql)` | 查询所有行 |
| `query(sql, params)` | 带参数查询 |
| `prepare(sql)` | 创建 Statement |
| `last_error()` | 最近错误 |
| `last_insert_rowid()` | 最近插入 id |
| `close()` | 关闭 |

`Statement` 常用方法：

| 方法 | 说明 |
| --- | --- |
| `step()` | 前进一步 |
| `column_count()` | 列数 |
| `column_name(index)` | 列名 |
| `column_value(index)` | 列值 |
| `row_as_dict()` | 当前行转字典 |
| `fetch_all()` | 取所有行 |
| `finalize()` | 释放 statement |

## table: 命令行表格

```bolide
import "std/table/table.bl" as table;

print(table.format_with_header(
    ["name", "lang"],
    [["bolide", "bl"], ["tool", "cli"]]
) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `format(rows)` | 格式化二维字符串列表 |
| `format_with_header(headers, rows)` | 带表头和分隔线 |

## template: HTML 模板

`std/template` 用 `dict<str, dynamic>` 渲染模板，适合 Web 页面和文本生成。

```bolide
import "std/template/template.bl" as template;

let ctx: dict<str, dynamic> = {
    "title": "Hello",
    "name": "Bolide"
};

print(template.render("<h1>{{title}}</h1><p>{{name}}</p>", ctx) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `escape_html(value)` | HTML 转义 |
| `render(source, context)` | 渲染字符串模板 |
| `render_file(path, context)` | 渲染文件模板 |

## text: 文本处理

```bolide
import "std/text/text.bl" as text;

let words: list<str> = text.words(" hello\tbolide\nstd ");
print(text.join(words, ",") + "\n");
print(text.snake("HelloBolide stdLib") + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `join(items, sep)` | 拼接字符串列表 |
| `lines(value)` | 按行切分，统一 CRLF |
| `words(value)` | 按空白切词 |
| `repeat(value, count)` | 重复 |
| `truncate(value, max_len, suffix)` | 截断 |
| `pad_left`、`pad_right` | 填充 |
| `indent(value, prefix)` | 给每行加前缀 |
| `snake(value)` | 转 snake_case |
| `kebab(value)` | 转 kebab-case |
| `slug(value)` | 生成 slug |

## time: 时间

```bolide
import "std/time/time.bl" as time;

let start: int = time.monotonic_ms();
time.sleep_ms(100);
print(str(time.monotonic_ms() - start) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `now()`、`unix()` | Unix 秒 |
| `now_ms()` | Unix 毫秒 |
| `now_us()` | Unix 微秒 |
| `monotonic_ms()` | 单调时钟毫秒，适合计时 |
| `sleep_ms(ms)` | 睡眠毫秒 |
| `sleep(seconds)` | 睡眠秒 |

## url: URL 处理

```bolide
import "std/url/url.bl" as url;

let u: url.Url = url.parse("https://example.com:443/a/b?q=bolide#top");
print(u.host + "\n");

let full: str = url.resolve("https://example.com/docs/index.html", "../api?q=1");
print(full + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `encode_component(value)` | URL 组件编码 |
| `encode_query_component(value)` | 查询参数编码，空格为 `+` |
| `decode_component(value)` | 解码 |
| `decode_query_component(value)` | 查询参数解码 |
| `parse(raw)` | 解析为 `Url` |
| `stringify(u)` | `Url` 转字符串 |
| `resolve(base_url, href)` | 解析相对链接 |
| `query_parse(query)` | 查询串转 dict |
| `query_build(values)` | dict 转查询串 |
| `pair(key, value)` | 创建查询参数 |
| `query_pairs(pairs)` | 保序生成查询串 |

`Url` 字段：`scheme`、`host`、`port`、`path`、`query`、`fragment`。

## uuid: UUID 和短 ID

```bolide
import "std/uuid/uuid.bl" as uuid;

let id: str = uuid.v4();
print(id + "\n");
print(str(uuid.is_v4(id)) + "\n");
print(uuid.short_id(12) + "\n");
```

常用 API：

| API | 说明 |
| --- | --- |
| `v4()` | UUID v4，带连字符 |
| `compact_v4()` | 32 位无连字符 v4 |
| `short_id(length)` | 字母数字短 ID |
| `is_v4(value)` | 校验 UUID v4 |

## web: HTTP 服务

`std/web` 是完整 Web 标准库，支持路由、静态文件、中间件、会话、CORS、压缩、TLS、流式响应、SSE 和 WebSocket。

最小服务：

```bolide
import "std/web/web.bl" as web;

fn home(req: web.Request) -> web.Response {
    return web.text("hello bolide");
}

let app: web.App = web.app();
app.get("/", home);
app.run("127.0.0.1", 8080);
```

JSON API：

```bolide
import "std/web/web.bl" as web;
import "std/json/json.bl" as json;

fn api(req: web.Request) -> web.Response {
    let body: str = json.object([
        json.pair("ok", json.value(true)),
        json.pair("path", json.value(req.path()))
    ]);
    return web.json(body);
}

let app: web.App = web.app();
web.get(app, "/api", api);
app.run("127.0.0.1", 8080);
```

请求常用方法：

| 方法 | 说明 |
| --- | --- |
| `method()`、`target()`、`path()`、`query()`、`version()` | 请求信息 |
| `path_param(name)` | 路由参数 |
| `query_param(name)` | 查询参数 |
| `header(name)` | 请求头 |
| `body_text()`、`body_bytes()` | 请求体 |
| `form_param(name)` | 表单字段 |
| `cookie(name)` | Cookie |
| `multipart_count()`、`part(index)`、`file(field)` | multipart |

响应常用函数：

| API | 说明 |
| --- | --- |
| `text(body)` | text/plain |
| `html(body)` | text/html |
| `json(body)` | application/json |
| `bytes_response(body, content_type)` | bytes 响应 |
| `empty(status)` | 空响应 |
| `redirect(location, status)` | 重定向 |
| `make_response(status, content_type, body)` | 自定义响应 |

`Response` 常用方法包括设置状态、header、cookie 和读取 body。具体以 `std/web/web.bl` 中 `Response` 类为准。

路由常用 API：

| API | 说明 |
| --- | --- |
| `app()` | 创建 App |
| `get/post/put/patch/delete/head/options/trace/connect` | 注册路由 |
| `*_async` | 注册异步处理路由 |
| `route(app, method, path, handler)` | 通用路由 |
| `static_files(app, url_prefix, dir)` | 静态文件 |
| `not_found(app, handler)` | 404 处理 |
| `error_handler(app, handler)` | 错误处理 |
| `use_before`、`use_after` | 中间件 |
| `enable_compression` | 压缩 |
| `enable_cors` | CORS |
| `tls`、`tls_pkcs8` | TLS |
| `run(host, port)` | 启动服务 |
| `serve(host, port, max_requests)` | 服务固定请求数，适合测试 |

客户端 API：

| API | 说明 |
| --- | --- |
| `fetch(method, url, body, headers)` | HTTP 客户端请求 |
| `fetch_with_options(method, url, body, headers, timeout_ms, max_redirects)` | 带选项请求 |

`ClientResponse` 常用方法：`status()`、`header(name)`、`body_text()`、`body_bytes()`、`error()`、`ok()`、`free()`。请求层错误不会静默丢失；此时 `status()` 为 `0`，`error()` 返回错误信息。

会话 API：

| API | 说明 |
| --- | --- |
| `session(req, res)` | 获取 Session |
| `session_set_ttl(ttl_seconds)` | 设置 TTL |
| `session_config(ttl_seconds, max_sessions, dir)` | 会话配置 |

## 常见组合

### 读取 CSV，过滤后输出表格

```bolide
import "std/fs/fs.bl" as fs;
import "std/csv/csv.bl" as csv;
import "std/table/table.bl" as table;

let rows: list<list<str>> = csv.parse(fs.read_text("users.csv"));
let out: list<list<str>> = [];

for row in rows {
    if row.len() >= 2 and row[1] != "" {
        out.push(row);
    }
}

print(table.format(out) + "\n");
```

### 调 API 并生成 JSON

```bolide
import "std/http/http.bl" as http;
import "std/json/json.bl" as json;

let req: str = json.object([
    json.pair("name", json.value("Bolide")),
    json.pair("count", json.value(3))
]);

let res: http.Response = http.post_json("https://example.com/api", req);
if res.ok() {
    print(res.body + "\n");
}
```

### 用缓存避免重复抓取

```bolide
import "std/cache/cache.bl" as cache;
import "std/crawler/crawler.bl" as crawler;

let seen: cache.Cache = cache.new();
let opts: crawler.Options = crawler.default_options();
let urls: list<str> = ["https://example.com/"];

for u in urls {
    if not seen.contains(u) {
        seen.set_ttl(u, true, 60000);
        let page: crawler.Page = crawler.fetch(u, opts);
        print(str(page.status) + " " + u + "\n");
    }
}
```

### Web 路由里渲染模板

```bolide
import "std/web/web.bl" as web;
import "std/template/template.bl" as template;

fn index(req: web.Request) -> web.Response {
    let ctx: dict<str, dynamic> = {
        "title": "Bolide",
        "message": "hello"
    };
    return web.html(template.render("<h1>{{title}}</h1><p>{{message}}</p>", ctx));
}

let app: web.App = web.app();
app.get("/", index);
app.run("127.0.0.1", 8080);
```

## 约定和注意事项

标准库模块通常采用 `import "std/name/name.bl" as name;` 形式导入。

返回 `bool` 的文件、网络和数据库函数通常表示操作是否成功；需要详细错误时查看对应对象的 `last_error()` 或响应状态码。

`dynamic` 适合在数据库、模板、缓存、会话等边界使用。取出后如果需要具体类型，可以用 `str(value)`、`int(value)` 等转换。

`std/json` 当前是生成器，不是完整 JSON 解析器。需要从 API 响应提取复杂 JSON 时，后续应补 JSON parser 或绑定 runtime 解析库。

`std/html` 是轻量提取工具，不是完整 DOM 解析器。复杂网页解析建议后续补更强 HTML parser。

`std/encoding` 处理 `bytes`，不负责字符串编码转换。

网络相关库可能阻塞当前线程。GUI 程序中请求网络应使用线程或异步方式，避免卡住界面。

涉及文件删除、目录递归删除和外部进程执行时，建议先打印目标路径或使用 `path.canonicalize` 确认范围。
