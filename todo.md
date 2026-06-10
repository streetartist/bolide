暂不支持：
  - ⏸️ 混合类型元组的打印（目前显示指针地址）

---

## 📌 AOT 与 JIT 功能对齐 (进行中)

JIT 为参考实现。tests/ 全量 AOT 扫描结果：61 通过 / 45 失败（JIT 为 106/106）。
失败按根因分组如下，按影响面排序：

### A. AOT 缺失运行时函数声明（编译报错 "xxx not found"）
| 缺失符号 | 影响 |
|---|---|
| `thread_join_int/_float/_ptr` | 所有 spawn/join 测试（~10 个） |
| `print_list` / `print_dict` / `print_tuple` | 容器直接打印（~7 个） |
| `select_wait_first` | async select |
| `coroutine_await_ptr` | await 指针类型结果 |
| `bigint_to_i64` / `decimal_from_i64` | 类型转换 |

修法：在 aot.rs `register_builtins` 按 JIT 的签名补声明（运行时 staticlib 中符号已存在）。

### B. AOT 缺失编译能力（编译报错）
- 列表/字典方法不全：`len`/`pop`/`contains` 等（JIT 的 compile_list_method_call 全集需移植）
- 类继承的方法调用（"Unknown method: get_age"）
- 模块函数调用（"Unknown method: add"，import 后的 module.func）
- 函数作为值/回调传参（"Function not found: f"，func_type 支持）
- hello.bl："Duplicate definition"（用户 `fn main` 与 AOT 合成 main 冲突，需改名合成符号）

### C. AOT 运行期崩溃（段错误，需逐个调试）
- bigint 算术路径：test_bigint_arith / _simple / _noprint / func_bigint
- test_rc / test_tuple_memory / test_coroutine_memory（RC 清理路径）
- test_name / test_param_modes / test_borrow_only：**内置函数名冲突**——
  JIT 已用 `@_` 前缀隔离内置符号，AOT 侧用户函数与运行时导出符号（C 链接名）
  仍共享命名空间，需对用户函数做符号改名（如 `bolide.user.<name>`，
  保留 `main` 不变），内部查找键不变

### D. 设计约束（AOT 链接模型）
- AOT 的运行时导入必须匹配 staticlib 的真实导出名（C 符号），无法使用 `@_`
  这类非法 C 标识符——因此 AOT 的隔离方向是改名用户函数（见 C 组）
Bolide 现在可以调用 C 库。让我总结当前能力：

  ✅ 已支持

  | 功能           | 示例                         |
  |----------------|------------------------------|
  | 调用 C 函数    | abs(-42) → 42                |
  | 基本类型       | c_int, c_double, i32, u64 等 |
  | 指针类型       | *void, *c_int                |
  | 函数指针回调   | fn(i64, i64) -> i64          |
  | 类型自动转换   | Bolide int ↔ C c_int         |
  | 跨平台调用约定 | Windows/Linux 自动适配       |

  ⚠️ 当前限制

  | 限制       | 说明                   |
  |------------|------------------------|
  | 结构体传值 | 暂不支持按值传递结构体 |
  | 可变参数   | printf 等暂不支持      |
  | 字符串     | 需手动处理 C 字符串    |
  | 复杂回调   | 闭包/捕获变量暂不支持  |

---

## 📌 Channel 对象传递语义 (待实现)

### 现状
- Channel 目前只支持 `i64` 值传递（int/float 的 Copy 语义）
- 不支持传递 string、bigint、class 实例等复杂对象

### 设计方案：Arc Clone 语义

```
发送: ch <- obj   →  Arc::clone() 增加引用计数
接收: let x = <- ch  →  获得 Arc 共享引用
```

### 性能评估
| 操作 | 开销 | 说明 |
|------|------|------|
| Arc::clone() | ~30 ns | 原子加引用计数，极快 |
| 深拷贝 1KB | ~500 ns | 需要 memcpy |
| **结论** | Arc 快 10x+ | ✅ 推荐方案 |

### 注意事项
- 共享对象需遵守不可变约定，避免发送后修改原对象
- 高争用场景 (64+ 线程) 可能出现缓存行 ping-pong
- 与 Bolide 现有 ARC 内存管理完美契合

### 实现要点
1. 扩展 `BolideChannel` 支持 `*mut c_void` 值类型
2. 发送时调用 `bolide_rc_retain()` 增加引用计数
3. 接收时返回指针，由接收方持有引用
4. 关闭通道时释放队列中未消费的对象

---

## 📌 高并发协程架构升级 (待实现)

### 现状分析
- 当前协程使用 **1:1 线程模型**（每个 async 创建 OS 线程）
- 100K 并发 = 100K 线程 = ~200GB 内存 ❌
- 适合 GUI/计算，不适合高并发网络服务

### 目标
支持 **100K+ 并发任务**，内存控制在 ~50MB

### 方案对比

| 方案 | 改动量 | 周期 | 性能 | 推荐 |
|------|--------|------|------|------|
| **Tokio 集成** | ~500 行 | 1-2 周 | 100x | ⭐⭐⭐⭐⭐ |
| 自研 M:N 调度 | ~2000 行 | 2-3 月 | 100x | ⭐⭐⭐ |
| Stackless 状态机 | ~5000 行 | 6+ 月 | 1000x | ⭐⭐ |

### 推荐方案：Tokio 集成

```toml
# Cargo.toml
tokio = { version = "1", features = ["rt-multi-thread"] }
```

```rust
// 核心改动 - coroutine.rs
static RUNTIME: Lazy<Runtime> = Lazy::new(|| Runtime::new().unwrap());

pub fn bolide_coroutine_spawn_int(func: fn() -> i64) -> *mut BolideFuture {
    RUNTIME.spawn_blocking(move || func())  // 替换 thread::spawn
}
```

### 实现步骤
1. 添加 tokio 依赖
2. 创建全局 Runtime
3. 替换 `thread::spawn` → `tokio::spawn_blocking`
4. （可选）添加真正的 async I/O 支持

### 后续扩展
- `await http_get(url)` - 异步 HTTP
- `await read_file(path)` - 异步文件 I/O
- `await sleep(ms)` - 异步定时器

---

## 📌 线程池性能优化 (待实现)

### 当前瓶颈
- 单一全局队列 + Mutex，高争用时成为瓶颈
- Worker 越多，锁争用越严重

### 优化方案对比

| 方案 | 改动量 | 性能提升 | 推荐 |
|------|--------|----------|------|
| **Rayon 替换** | ~50 行 | 5-10x | ⭐⭐⭐⭐⭐ |
| Work-Stealing | ~300 行 | 5-10x | ⭐⭐⭐⭐ |
| 无锁队列 | ~200 行 | 2-3x | ⭐⭐⭐ |
| CPU 亲和性 | ~20 行 | 1.1-1.3x | ⭐⭐⭐ |

### 推荐方案：Rayon

```toml
# Cargo.toml
rayon = "1.10"
```

```rust
// 替换线程池实现
rayon::spawn(|| expensive_computation());
```

### Work-Stealing 备选

```toml
crossbeam-deque = "0.8"
```

```rust
// 每个 Worker 有本地队列，空闲时从其他 Worker 偷任务
// 减少锁争用，提升 5-10 倍性能
```

### 性能提升预期
| Workers | 当前 (Mutex) | 优化后 |
|---------|--------------|--------|
| 4 | ~200 ns/task | ~50 ns/task |
| 8 | ~500 ns/task | ~80 ns/task |
| 16 | ~2000 ns/task | ~100 ns/task |