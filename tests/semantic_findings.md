# Bolide Semantic Test Findings

Date: 2026-06-17

This note records the semantic regression tests added under `tests/test_semantic_*.bl`
and the semantic issues found while running them.

## How to Run

```powershell
Get-ChildItem tests\test_semantic_*.bl | ForEach-Object {
    Write-Host "== $($_.Name) =="
    .\bolide.exe run $_.FullName
}
```

Each test prints `ok ...` for passing checks and `FAIL ... got=... want=...`
for semantic mismatches.

## Added Tests

- `test_semantic_scope_shadow.bl`: block, loop, and global name resolution.
- `test_semantic_short_circuit.bl`: `and` / `or` short-circuit evaluation.
- `test_semantic_closure_mutation.bl`: closure capture and mutable captured state.
- `test_semantic_defaults_eval.bl`: default args, named args, spreads, and kwargs.
- `test_semantic_dispatch_super.bl`: override dispatch, inherited fields, and `super`.
- `test_semantic_container_alias_slice.bl`: list/dict assignment-copy and slice behavior.
- `test_semantic_match_nested.bl`: nested ADT match and pattern binding scope.
- `test_semantic_try_finally.bl`: `try` / `catch` / `finally` execution order.
- `test_semantic_comprehension_capture.bl`: list comprehensions with captures/shadowing.
- `test_semantic_numeric_mixed.bl`: numeric conversions and dynamic arithmetic.

## Findings

### Fixed in JIT

- `and` / `or` did not short-circuit. The right-hand side was evaluated even when
  the left-hand side determines the result.
- Inner `let` declarations overwrote outer variables with the same name instead
  of creating block-local bindings.
- `for` loop induction variables overwrote existing outer variables with the
  same name.
- List-comprehension iteration variables overwrote existing outer variables.
- `match` pattern bindings overwrote outer variables with the same name.
- Closures did not preserve mutable captured local state across calls.
- A temporary list used as a list-comprehension iterator could be released before
  the comprehension finished iterating.
- `dynamic + dynamic`, followed by `int(...)`, returned a pointer-like large
  number instead of the expected numeric result.

### Remaining Larger Design Issue

- Base-class methods appear to call base implementations directly instead of
  dynamically dispatching through overridden `self` methods. Fixing this needs
  object layout/runtime support for class identity or method tables; the current
  object header only stores reference counts and data size.

## Passing Coverage

- Scope shadowing for block, loop, comprehension, and match pattern bindings.
- `and` / `or` short-circuit behavior.
- Mutable closure capture state across calls.
- Dynamic arithmetic followed by conversion.
- Default argument evaluation and skipping provided defaults.
- Named arguments, `*list` spread arguments, and `**dict` kwargs.
- Nested ADT recursion for non-shadowing cases.
- `try` / `catch` / `finally` execution order.
- Basic slice-copy behavior for list slices.
- List/dict assignment-copy behavior.
- Numeric conversion basics for `int`, `bigint`, and `decimal`.
