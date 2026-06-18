# Bolide Semantic Test Findings

Date: 2026-06-18

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
- `test_semantic_branch_cleanup.bl`: `if` / `else` arm-local scope cleanup.
- `test_semantic_elif_shadow.bl`: `elif` branch scope cleanup and outer-name preservation.
- `test_semantic_catch_shadow.bl`: `catch` binding shadowing and outer-name preservation.
- `test_semantic_finally_controlflow.bl`: `finally` on `break` / `continue` / `return`.
- `test_semantic_loop_exit_shadow.bl`: loop-body shadow cleanup on `continue` / `break`.
- `test_semantic_rethrow_finally.bl`: nested `try` rethrow and `finally` order.
- `test_semantic_exception_hierarchy.bl`: subclass exception matching and catch order.
- `test_semantic_call_eval_order.bl`: positional/named argument evaluation order.
- `test_semantic_variadic_eval_order.bl`: `*args` / `**kwargs` evaluation order.
- `test_semantic_method_variadic_eval_order.bl`: method-call `*args` / `**kwargs` evaluation order.
- `test_semantic_method_named_eval.bl`: method named/default argument evaluation order.
- `test_semantic_ref_named_eval.bl`: `ref` parameter slot binding under named-argument reordering.
- `test_semantic_indirect_named_eval.bl`: indirect callable evaluation paths.
- `test_semantic_func_value_branch_flow.bl`: function values through branches and repeated returns.
- `test_semantic_closure_escape_chain.bl`: escaped closures through pass-throughs, lists, and object fields.
- `test_semantic_func_value_containers.bl`: function values in list/dict/tuple containers and field reassignment.
- `test_semantic_literal_assign_eval_order.bl`: list/dict literals and indexed assignment evaluation order.
- `test_semantic_comprehension_capture.bl`: list comprehensions with captures/shadowing.
- `test_semantic_numeric_mixed.bl`: numeric conversions and dynamic arithmetic.

## Findings

### Fixed in JIT and AOT

The full `tests/test_semantic_*.bl` suite now passes in both JIT `run` mode and
AOT `compile` mode.

- `and` / `or` did not short-circuit. The right-hand side was evaluated even when
  the left-hand side determines the result.
- Inner `let` declarations overwrote outer variables with the same name instead
  of creating block-local bindings.
- `for` loop induction variables overwrote existing outer variables with the
  same name.
- List-comprehension iteration variables overwrote existing outer variables.
- `match` pattern bindings overwrote outer variables with the same name.
- `catch` bindings and branch-local names are now covered with targeted regression tests.
- `elif` branch locals, loop-exit cleanup, and nested rethrow ordering are now covered too.
- Exception hierarchy dispatch and call evaluation order are now covered too.
- Spread / kw-spread call expressions were evaluated after later explicit arguments instead of
  at their source positions.
- A function value returned from another function could be misclassified at call sites and crash
  during indirect invocation; direct and stored indirect calls are now covered too.
- Escaped closures stored in `list<func...>` or object `func` fields could lose ownership and crash
  when fetched and called; function-typed containers and fields now retain/release closure objects.
- JIT dictionary literals had a stale local type-tag mapping for `func` values, so
  `dict<str, func...>` could retain/release entries with the wrong element tag.
- Raw named functions stored into function-typed containers are wrapped with a no-capture closure
  adapter so the stored representation is consistent with escaped closures.
- Closures did not preserve mutable captured local state across calls.
- A temporary list used as a list-comprehension iterator could be released before
  the comprehension finished iterating.
- `dynamic + dynamic`, followed by `int(...)`, returned a pointer-like large
  number instead of the expected numeric result.
- Base-class methods now dispatch through overridden `self` methods using a
  runtime class tag stored in the object header, while `super` keeps static
  parent dispatch.

## Passing Coverage

- Scope shadowing for block, loop, comprehension, and match pattern bindings.
- `and` / `or` short-circuit behavior.
- Mutable closure capture state across calls.
- Dynamic arithmetic followed by conversion.
- Default argument evaluation and skipping provided defaults.
- Named arguments, `*list` spread arguments, and `**dict` kwargs.
- Nested ADT recursion for non-shadowing cases.
- `try` / `catch` / `finally` execution order.
- `if` / `else` arm-local cleanup and `catch` shadowing.
- `finally` execution on `break`, `continue`, and `return`.
- `elif` branch cleanup, loop-exit shadow cleanup, and nested rethrow `finally` order.
- Exception subclass matching and positional/named call evaluation order.
- `*args` / `**kwargs` source-order evaluation for functions and methods.
- Method named/default argument evaluation and `ref` named-argument binding.
- Stored and directly returned function-value calls.
- Branch-selected function values and function values relayed through multiple returns.
- Escaped closures fetched from lists and object fields.
- Function values in lists, dictionaries, tuples, and function-typed field reassignment.
- List/dict literal and indexed-assignment evaluation order.
- Basic slice-copy behavior for list slices.
- List/dict assignment-copy behavior.
- Numeric conversion basics for `int`, `bigint`, and `decimal`.
- Dynamic base-method dispatch through inherited overrides and `super`.
