# 装饰器与上下文管理器

## 装饰器（Python 自然形态）

### 协议

```bolide
// deco(f) -> f′
fn logged(f: func() -> int) -> func() -> int {
    return fn() -> int {
        print("before");
        let r = f();
        print("after");
        return r;
    };
}

@logged
fn answer() -> int { return 42; }
// 语义：answer = logged(answer)
```

工厂：

```bolide
@repeat(3)
fn step() -> int { ... }
// 语义：step = repeat(3)(step)
```

多层：`@a @b fn f` ⇒ `f = a(b(f))`。

### 与 `@` 宏的优先级

1. `@test` / `@inline` / `@export`
2. `attr macro`（编译期）
3. 否则 → 运行时装饰器

宏调用始终是 `name!`，与 `@` 无关。

### 实现要点

- 脱糖为 `__raw_*` + 包装函数；包装体内用 `let` 承接中间闭包，避免链式调用临时生命周期问题。
- 闭包可捕获 `func(...)`（JIT：捕获按闭包 ABI 调用；传参时裸函数 wrap 为闭包）。

## 上下文管理器 `with`

```bolide
with expr as name { body }
with a() as x, b() as y { body }
```

协议：`enter()` / `exit()`（`finally` 保证 `exit`）。

## 测试

- `tests/test_decorator_with.bl`
- `tests/test_hof_capture.bl`（捕获 func 的回归）
